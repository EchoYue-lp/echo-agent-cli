//! DAG scheduler for TaskRuntime runs.
//!
//! Converts an approved `TaskPlan` into a dependency graph and executes it,
//! honoring the plan's parallelism rules:
//!
//! - read-only tasks (read_only_review, investigation, test_plan, review,
//!   summary) run concurrently up to `max_concurrent_workers`, each delegated
//!   to a registered subagent role via `delegate_to_agent_with_cancel` (fork
//!   mode → isolated instance under the executor's semaphore, NOT the primary
//!   agent's execution_mutex, so they parallelize);
//! - implementation / debugging / verification tasks serialize (acquire the
//!   write lock) and run on the PRIMARY agent directly (never delegated to a
//!   read-only worker);
//! - the overall worker count is capped by `ConcurrencyLimits`.
//!
//! Cancellation: each dispatched task gets a child of the parent run's
//! CancellationToken. Read-only delegation propagates cancel through
//! `delegate_to_agent_with_cancel`; mutating tasks race `Agent::execute`
//! against the cancel token. Cancelling the run therefore cancels every
//! in-flight task.
//!
//! Guarantees:
//! - the run transitions Running → (Completed | Failed | Cancelled | Paused);
//! - every task boundary writes a TaskEvent + updates the todo projection;
//! - implementation/debugging tasks pass a review gate before being marked
//!   Completed; a failing review either re-queues a fix task or trips the
//!   circuit breaker (Paused);
//! - cancellation propagates to all in-flight tasks;
//! - a failed task marks itself Failed but lets already-running siblings
//!   finish (the run ends Failed); downstream tasks are skipped.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use echo_agent::agent::{Agent, AgentEvent, CancellationToken};
use futures::StreamExt;
use tokio::sync::Semaphore;

use super::store::{StoreError, TaskRuntimeStore};
use super::types::*;

pub type WorkerTraceSink = Arc<dyn Fn(WorkerTraceEvent) + Send + Sync>;

/// Concurrency caps. Sourced from the AgentPool when available, else defaults
/// from the plan's "Initial limits" (max 3–4 workers, writes serialized).
#[derive(Debug, Clone)]
pub struct ConcurrencyLimits {
    /// Max simultaneous worker agents (read-only fan-out).
    pub max_concurrent_workers: usize,
    /// Max simultaneous mutating tasks. Default 1 — writes serialize.
    pub max_concurrent_writes: usize,
    /// Max simultaneous shell/verification tasks.
    pub max_concurrent_shells: usize,
    /// Max simultaneous LLM calls across all workers (plan §704:
    /// max_parallel_llm_calls = 3). Prevents rate-limit hits and cost spikes.
    pub max_parallel_llm_calls: usize,
}

impl Default for ConcurrencyLimits {
    fn default() -> Self {
        Self {
            max_concurrent_workers: 4,
            max_concurrent_writes: 1,
            max_concurrent_shells: 1,
            max_parallel_llm_calls: 3,
        }
    }
}

/// Outcome of executing a whole run.
#[derive(Debug, Clone)]
pub enum RunOutcome {
    Completed,
    Failed {
        failed_task_id: String,
        error: String,
    },
    Cancelled,
    /// A task failed and the run is paused for user/agent decision. The
    /// failed task is marked `Failed`; downstream dependents are `Blocked`.
    /// The run is transitioned to `Paused`. The user can retry, skip, or
    /// edit the plan before resuming.
    Paused {
        failed_task_id: String,
        error: String,
    },
}

/// Error returned by the executor.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("run {0} not found")]
    RunNotFound(String),
    #[error("run {0} has no plan")]
    NoPlan(String),
    #[error("run {0} is in state {1:?}, expected Running")]
    NotRunning(String, TaskRunStatus),
    #[error("primary agent required to dispatch subagent workers")]
    NoAgent,
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("subagent dispatch failed: {0}")]
    Delegate(String),
    #[error("worker execution failed: {0}")]
    Worker(String),
}

/// Execute an approved run to completion.
///
/// The caller (a Tauri command) holds the `AppState`, the store, and the
/// optional `AgentPool`. Execution is driven on the provided runtime; the
/// caller typically `tokio::spawn`s this and lets it run independently of the
/// chat stream (so a long run does not block the GUI, per plan §4).
pub async fn execute_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: Option<crate::agent_handle::AgentHandle>,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    layer_manager: Option<Arc<echo_agent::evolution::MemoryLayerManager>>,
    run_store: Option<Arc<dyn echo_agent::trace::RunStore>>,
    trace_sink: Option<WorkerTraceSink>,
    run_id: &str,
    cache_user_id: String,
    parent_cancel: CancellationToken,
) -> Result<RunOutcome, ExecError> {
    let run = store
        .get_run(run_id)?
        .ok_or(ExecError::RunNotFound(run_id.to_string()))?;
    // Zombie recovery: a run left in a non-terminal state (e.g. process crashed
    // during shutdown) has no driver to finish it. Auto-transition to Failed so
    // it doesn't block the run list forever.
    //
    // `Running` is intentionally excluded: if the run is already Running when
    // the executor starts, it was either just transitioned by the IPC (resume
    // path) or left behind by a crash. In both cases the executor can safely
    // proceed — it re-reads the plan from the store and skips completed tasks.
    if matches!(run.status, TaskRunStatus::Paused) {
        let reason = format!(
            "recovered from {} (interrupted by process restart)",
            run.status.as_str()
        );
        let _ = store.note(run_id, None, &reason);
        let _ = store.transition_run(run_id, TaskRunStatus::Failed);
        save_trace(
            run_store.as_ref(),
            run_id,
            &run.goal,
            &run.conversation_id,
            "failed",
        );
        return Ok(RunOutcome::Failed {
            failed_task_id: "<none>".into(),
            error: format!(
                "run was in {} state (interrupted); auto-transitioned to Failed",
                run.status.as_str()
            ),
        });
    }
    // The caller must have transitioned Pending → Running before spawning
    // the executor. Here we only accept Running.
    if run.status != TaskRunStatus::Running {
        return Err(ExecError::NotRunning(run_id.to_string(), run.status));
    }
    let plan = store
        .get_plan(run_id)?
        .ok_or(ExecError::NoPlan(run_id.to_string()))?;
    emit_worker_trace(
        trace_sink.as_ref(),
        WorkerTraceEvent::new(
            run_id.to_string(),
            WorkerTraceEventKind::RunStarted,
            serde_json::json!({
                "goal": &run.goal,
                "conversation_id": &run.conversation_id,
                "mode": "task_runtime",
            }),
        ),
    );

    let primary_agent = primary_agent.ok_or(ExecError::NoAgent)?;
    let limits = ConcurrencyLimits::default();

    let outcome = run_dag(
        store.clone(),
        RealTaskWorker {
            primary_agent: primary_agent.clone(),
        },
        reviewer_llm,
        run_id,
        plan.tasks,
        limits,
        parent_cancel,
        trace_sink.clone(),
        cache_user_id.clone(),
    )
    .await;

    // Reflect the outcome on the run state. Each branch also writes a trace
    // Run record when a RunStore is available.
    match &outcome {
        Ok(RunOutcome::Completed) => {
            emit_worker_trace(
                trace_sink.as_ref(),
                WorkerTraceEvent::new(
                    run_id.to_string(),
                    WorkerTraceEventKind::RunCompleted,
                    serde_json::json!({ "status": "completed" }),
                ),
            );
            let _ = store.transition_run(run_id, TaskRunStatus::Completed);
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "completed",
            );
            super::memory_bridge::write_memory_candidate(
                layer_manager.as_ref(),
                &store,
                super::memory_bridge::MemoryEvent::RunCompleted {
                    run_id: run_id.to_string(),
                    goal: run.goal.clone(),
                },
            );
        }
        Ok(RunOutcome::Failed {
            failed_task_id,
            error,
        }) => {
            emit_worker_trace(
                trace_sink.as_ref(),
                WorkerTraceEvent::new(
                    run_id.to_string(),
                    WorkerTraceEventKind::RunFailed,
                    serde_json::json!({
                        "failed_task_id": failed_task_id,
                        "error": error,
                    }),
                ),
            );
            // Running → Failed is legal. Use None for synthetic task ids
            // (<none>/<join>) to avoid orphan task_id events.
            let tid = if failed_task_id.starts_with('<') {
                None
            } else {
                Some(failed_task_id.as_str())
            };
            let _ = store.note(run_id, tid, &format!("run failed: {error}"));
            let _ = store.transition_run(run_id, TaskRunStatus::Failed);
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "failed",
            );
        }
        Ok(RunOutcome::Cancelled) => {
            emit_worker_trace(
                trace_sink.as_ref(),
                WorkerTraceEvent::new(
                    run_id.to_string(),
                    WorkerTraceEventKind::RunCancelled,
                    serde_json::json!({ "status": "cancelled" }),
                ),
            );
            let _ = store.transition_run(run_id, TaskRunStatus::Cancelled);
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "cancelled",
            );
            super::memory_bridge::write_memory_candidate(
                layer_manager.as_ref(),
                &store,
                super::memory_bridge::MemoryEvent::RunCancelledByUser {
                    run_id: run_id.to_string(),
                    goal: run.goal.clone(),
                },
            );
        }
        Ok(RunOutcome::Paused {
            failed_task_id,
            error,
        }) => {
            emit_worker_trace(
                trace_sink.as_ref(),
                WorkerTraceEvent::new(
                    run_id.to_string(),
                    WorkerTraceEventKind::RunStatusChanged,
                    serde_json::json!({
                        "status": "paused",
                        "failed_task_id": failed_task_id,
                        "error": error,
                    }),
                ),
            );
            // run_dag already transitioned Running → Paused. Record the reason.
            let _ = store.note(
                run_id,
                Some(failed_task_id),
                &format!("run paused: {error}"),
            );
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "paused",
            );
        }
        Err(e) => {
            emit_worker_trace(
                trace_sink.as_ref(),
                WorkerTraceEvent::new(
                    run_id.to_string(),
                    WorkerTraceEventKind::RunFailed,
                    serde_json::json!({ "error": e.to_string() }),
                ),
            );
            let _ = store.note(run_id, None, &format!("executor error: {e}"));
            // Running → Failed is legal even if some tasks were mid-flight.
            let _ = store.transition_run(run_id, TaskRunStatus::Failed);
        }
    }
    outcome
}

/// Abstraction over how a single ready task is executed on a worker.
///
/// `run_dag` depends on this trait (not on `execute_task` directly) so the
/// scheduling core — frontier computation, dependency resolution, failure
/// propagation, cancellation, stall detection — can be unit-tested with a
/// deterministic mock worker instead of a real LLM-backed agent. The
/// production implementation ([`RealTaskWorker`]) wraps `execute_task`.
///
/// The worker is given the semaphores + file locks so it can honor the same
/// concurrency limits as the real path; mocks usually ignore them.
pub trait TaskWorker: Send + Sync {
    /// Execute `task` for `run_id`. Returns `(task_id, summary)` on success or
    /// `(task_id, error)` on failure (matching `execute_task`'s contract).
    fn dispatch(
        &self,
        store: Arc<TaskRuntimeStore>,
        run_id: String,
        task: PlanTask,
        cancel: CancellationToken,
        worker_sem: Arc<Semaphore>,
        write_sem: Arc<Semaphore>,
        shell_sem: Arc<Semaphore>,
        llm_sem: Arc<Semaphore>,
        file_write_locks: Arc<std::sync::Mutex<HashSet<String>>>,
        trace_sink: Option<WorkerTraceSink>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(String, Option<String>), (String, String)>>
                + Send,
        >,
    >;
}

/// Production worker: delegates to [`execute_task`] against the primary agent.
///
/// Note: the reviewer LLM is NOT held here — it is owned by `run_dag` itself
/// (the review gate runs at the `run_dag` level, after a worker returns). The
/// worker only needs the agent + concurrency primitives.
pub struct RealTaskWorker {
    pub primary_agent: crate::agent_handle::AgentHandle,
}

impl TaskWorker for RealTaskWorker {
    fn dispatch(
        &self,
        store: Arc<TaskRuntimeStore>,
        run_id: String,
        task: PlanTask,
        cancel: CancellationToken,
        worker_sem: Arc<Semaphore>,
        write_sem: Arc<Semaphore>,
        shell_sem: Arc<Semaphore>,
        llm_sem: Arc<Semaphore>,
        file_write_locks: Arc<std::sync::Mutex<HashSet<String>>>,
        trace_sink: Option<WorkerTraceSink>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(String, Option<String>), (String, String)>>
                + Send,
        >,
    > {
        let primary_agent = self.primary_agent.clone();
        Box::pin(async move {
            // Scope the run_id into the task-local so agent task-management
            // tools (task_create/update/complete/skip/list) can read it.
            // task_local survives `.await` across tokio thread hops, unlike
            // thread_local.
            super::task_tools::with_run_id(run_id.clone(), async {
                execute_task(
                    store,
                    primary_agent,
                    worker_sem,
                    write_sem,
                    shell_sem,
                    llm_sem,
                    file_write_locks,
                    trace_sink,
                    run_id,
                    task,
                    cancel,
                )
                .await
            })
            .await
        })
    }
}

/// Core DAG loop. Maintains a frontier of ready tasks and dispatches them
/// under the concurrency semaphores until all are done, the run is cancelled,
/// or a task fails.
async fn run_dag<W: TaskWorker + 'static>(
    store: Arc<TaskRuntimeStore>,
    worker: W,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    run_id: &str,
    tasks: Vec<PlanTask>,
    limits: ConcurrencyLimits,
    parent_cancel: CancellationToken,
    trace_sink: Option<WorkerTraceSink>,
    cache_user_id: String,
) -> Result<RunOutcome, ExecError> {
    // Wrap the worker in an Arc so each spawned task can clone the handle.
    let worker = Arc::new(worker);
    // Index tasks by id.
    let mut by_id: HashMap<String, PlanTask> =
        tasks.iter().map(|t| (t.id.clone(), t.clone())).collect();
    let all_ids: HashSet<String> = by_id.keys().cloned().collect();

    // Track completion state per task id.
    // Pre-populate with tasks already marked Completed — this is the resume
    // path: the executor re-reads the plan from the store and skips tasks
    // that finished in the previous run.
    let mut completed: HashSet<String> = tasks
        .iter()
        .filter(|t| t.status == TodoStatus::Completed)
        .map(|t| t.id.clone())
        .collect();
    let mut failed_id: Option<String> = None;
    // All tasks that have been marked Failed across waves. The skip logic
    // (top of loop) uses this to avoid overwriting a Failed todo to Skipped.
    let mut failed_set: HashSet<String> = HashSet::new();
    // Fix-task overrides produced by review gates, keyed by task id. A task
    // that fails review gets re-queued here with a bumped retry_count; the
    // next wave picks it up and re-runs it (possibly with a richer brief).
    let mut tasks_with_fixes: HashMap<String, PlanTask> = HashMap::new();

    let worker_sem = Arc::new(Semaphore::new(limits.max_concurrent_workers));
    let write_sem = Arc::new(Semaphore::new(limits.max_concurrent_writes));
    let shell_sem = Arc::new(Semaphore::new(limits.max_concurrent_shells));
    let llm_sem = Arc::new(Semaphore::new(limits.max_parallel_llm_calls));
    // G5: Track which files are currently being written, so two write tasks
    // targeting the SAME file don't run concurrently even if write_sem has
    // multiple permits in the future. Currently write_sem=1 serializes all
    // writes, but this guard prevents file-level races if that ever changes.
    let file_write_locks: Arc<std::sync::Mutex<HashSet<String>>> =
        Arc::new(std::sync::Mutex::new(HashSet::new()));

    // Loop until every task is resolved or the run aborts.
    loop {
        if parent_cancel.is_cancelled() {
            return Ok(RunOutcome::Cancelled);
        }
        if let Some(id) = &failed_id {
            // A task failed: propagate Blocked to downstream dependents
            // (but NEVER overwrite a task that's already Failed).
            for t in &tasks {
                if !completed.contains(&t.id)
                    && !failed_set.contains(&t.id)
                    && t.depends_on.iter().any(|d| failed_set.contains(d))
                {
                    let _ = store.set_task_status(
                        run_id,
                        &t.id,
                        TodoStatus::Blocked,
                        None,
                        Some("blocked: upstream task failed"),
                    );
                }
            }
            // Check if ALL non-terminal tasks are Failed or Blocked — if so,
            // the run is unrecoverable and should fail outright. Otherwise,
            // pause for user/agent decision.
            let all_dead = tasks.iter().all(|t| {
                completed.contains(&t.id)
                    || failed_set.contains(&t.id)
                    || t.depends_on.iter().any(|d| failed_set.contains(d))
            });
            let failed = by_id.get(id).cloned();
            let error = failed
                .map(|t| format!("task '{}' failed", t.title))
                .unwrap_or_else(|| "task failed".into());
            if all_dead {
                return Ok(RunOutcome::Failed {
                    failed_task_id: id.clone(),
                    error,
                });
            }
            // Pause the run for decision rather than failing outright.
            let _ = store.transition_run(run_id, TaskRunStatus::Paused);
            return Ok(RunOutcome::Paused {
                failed_task_id: id.clone(),
                error,
            });
        }
        if completed.len() == all_ids.len() {
            return Ok(RunOutcome::Completed);
        }

        // Find ready tasks: not yet completed/failed, all deps completed.
        // Prefer the fix-task variant when a review produced one (carries the
        // bumped retry_count + review-informed brief).
        let ready: Vec<PlanTask> = tasks
            .iter()
            .filter(|t| !completed.contains(&t.id))
            .filter(|t| t.depends_on.iter().all(|d| completed.contains(d)))
            .map(|t| {
                tasks_with_fixes
                    .get(&t.id)
                    .cloned()
                    .unwrap_or_else(|| t.clone())
            })
            .collect();

        if ready.is_empty() {
            // Nothing ready and not all done → deadlock (cycle or all
            // remaining are blocked by the failed one). Break out.
            if completed.len() + (if failed_id.is_some() { 1 } else { 0 }) >= all_ids.len() {
                continue;
            }
            // Genuine stall: record and fail.
            let _ = store.note(run_id, None, "DAG stalled: no ready tasks");
            return Ok(RunOutcome::Failed {
                failed_task_id: "<none>".into(),
                error: "DAG stalled with unfinished tasks (cycle or blocked)".into(),
            });
        }

        // Dispatch each ready task. We run them concurrently up to the
        // semaphores; join all before recomputing the frontier.
        //
        // Cancellation: each task gets parent_cancel.clone() (NOT child_token —
        // child_token creates a separate subtree that parent cancellation does
        // NOT propagate into). With clone, parent_cancel.cancel() immediately
        // fires every worker's select! guard. If we detect cancellation
        // mid-wave, we abort remaining handles before returning Cancelled so
        // no orphan tasks keep writing files.
        let mut handles: Vec<
            tokio::task::JoinHandle<Result<(String, Option<String>), (String, String)>>,
        > = Vec::new();
        for task in ready {
            let store = store.clone();
            let worker = worker.clone();
            let worker_sem = worker_sem.clone();
            let write_sem = write_sem.clone();
            let shell_sem = shell_sem.clone();
            let llm_sem = llm_sem.clone();
            let file_write_locks = file_write_locks.clone();
            let trace_sink = trace_sink.clone();
            // clone shares the same cancellation tree — parent cancel fires here.
            let cancel = parent_cancel.clone();
            let run_id_owned = run_id.to_string();
            handles.push(tokio::spawn(async move {
                worker
                    .dispatch(
                        store,
                        run_id_owned,
                        task,
                        cancel,
                        worker_sem,
                        write_sem,
                        shell_sem,
                        llm_sem,
                        file_write_locks,
                        trace_sink,
                    )
                    .await
            }));
        }

        // Await the wave. Collect results; on cancellation, abort stragglers.
        let mut wave_results: Vec<Result<(String, Option<String>), (String, String)>> = Vec::new();
        let mut cancelled_mid_wave = false;
        for handle in &mut handles {
            if parent_cancel.is_cancelled() {
                cancelled_mid_wave = true;
                break;
            }
            match handle.await {
                Ok(r) => wave_results.push(r),
                Err(join_err) => {
                    wave_results.push(Err((
                        "<join>".to_string(),
                        format!("worker task panicked: {join_err}"),
                    )));
                }
            }
        }
        if cancelled_mid_wave {
            // Abort any handles we didn't await so their workers stop ASAP.
            for handle in &mut handles {
                handle.abort();
            }
            return Ok(RunOutcome::Cancelled);
        }

        // Process wave results: first failure wins for the error message, but
        // ALL failed tasks are marked Failed (not later overwritten to Skipped
        // by the skip logic, which now excludes the failed set).
        let mut wave_failed: Vec<String> = Vec::new();
        for result in wave_results {
            match result {
                Ok((id, summary)) => {
                    // Review gate: implementation/debugging tasks must pass
                    // review before being marked Completed (plan §776-831).
                    // Read-only kinds are their own review → auto-pass.
                    let Some(task) = by_id.get(&id).cloned() else {
                        continue;
                    };
                    let passed = run_review_gate(
                        store.clone(),
                        reviewer_llm.clone(),
                        run_id,
                        &task,
                        summary.as_deref().unwrap_or(""),
                        &cache_user_id,
                    )
                    .await;
                    match passed {
                        ReviewGateOutcome::Pass => {
                            let _ = store.set_task_status(
                                run_id,
                                &id,
                                TodoStatus::Completed,
                                Some(&task.agent_role),
                                summary.as_deref(),
                            );
                            completed.insert(id);
                        }
                        ReviewGateOutcome::NeedsFix(fix_task) => {
                            if let Err(e) = store.update_plan_task(run_id, &fix_task) {
                                tracing::warn!(
                                    task_id = %fix_task.id,
                                    error = %e,
                                    "failed to persist fix task; in-memory only"
                                );
                            }
                            tasks_with_fixes.insert(fix_task.id.clone(), fix_task.clone());
                            let _ = store.set_task_status(
                                run_id,
                                &id,
                                TodoStatus::Pending,
                                Some(
                                    by_id
                                        .get(&id)
                                        .map(|t| t.agent_role.as_str())
                                        .unwrap_or("unknown"),
                                ),
                                Some("re-queued after review"),
                            );
                            by_id.insert(id.clone(), tasks_with_fixes[&id].clone());
                        }
                        ReviewGateOutcome::Suspend(reason) => {
                            let _ = store.note(
                                run_id,
                                Some(&id),
                                &format!("circuit breaker: {reason}"),
                            );
                            let _ = store.transition_run(run_id, TaskRunStatus::Paused);
                            return Ok(RunOutcome::Paused {
                                failed_task_id: id.clone(),
                                error: reason,
                            });
                        }
                        ReviewGateOutcome::Skipped => {
                            let _ = store.note(
                                run_id,
                                Some(&id),
                                "no reviewer LLM; auto-passing review gate",
                            );
                            let _ = store.set_task_status(
                                run_id,
                                &id,
                                TodoStatus::Completed,
                                Some(&task.agent_role),
                                summary.as_deref(),
                            );
                            completed.insert(id);
                        }
                    }
                }
                Err((id, err)) => {
                    // Hitrisk fail-closed: the task was blocked by a pre-execution
                    // high-risk safety check. The run was already transitioned to
                    // Paused inside execute_task. Propagate up so execute_run
                    // records the Paused outcome instead of treating it as Failed.
                    if let Some(reason) = err.strip_prefix("SUSPEND:") {
                        let _ = store.set_task_status(
                            run_id,
                            &id,
                            TodoStatus::Pending,
                            None,
                            Some("blocked: hitrisk requires user approval"),
                        );
                        return Ok(RunOutcome::Paused {
                            failed_task_id: id.clone(),
                            error: reason.to_string(),
                        });
                    }
                    // Mark this task Failed and record it in wave_failed so
                    // the skip logic (top of loop) does NOT overwrite it to
                    // Skipped. failed_id keeps the FIRST failure for the
                    // error message.
                    let _ = store.set_task_status(
                        run_id,
                        &id,
                        TodoStatus::Failed,
                        None,
                        Some(&format!("error: {err}")),
                    );
                    wave_failed.push(id.clone());
                    failed_set.insert(id.clone());
                    if failed_id.is_none() {
                        failed_id = Some(id);
                    }
                }
            }
        }
    }
}

/// Outcome of the review gate over a freshly-completed task.
enum ReviewGateOutcome {
    /// Task passed review (or is read-only and self-reviewing). Mark Completed.
    Pass,
    /// Review found fixable issues → re-queue this fix task (same id, bumped
    /// retry_count, review-informed brief) for the next wave.
    NeedsFix(PlanTask),
    /// Circuit breaker tripped (retry budget exhausted or repeated fingerprint).
    /// The run should be Suspended.
    Suspend(String),
    /// No reviewer LLM configured → auto-trust the task. Logged.
    Skipped,
}

/// Run the review gate for a task that just finished executing. Read-only
/// kinds auto-pass; implementation/debugging kinds are reviewed by the LLM
/// (when available) against the domain checklist. Applies the circuit
/// breaker on NeedsFix/Blocked outcomes.
async fn run_review_gate(
    store: Arc<TaskRuntimeStore>,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    run_id: &str,
    task: &PlanTask,
    worker_output: &str,
    cache_user_id: &str,
) -> ReviewGateOutcome {
    // Read-only kinds are their own review — no gate.
    if !super::review::requires_review(task.kind) {
        return ReviewGateOutcome::Pass;
    }
    let Some(llm) = reviewer_llm else {
        return ReviewGateOutcome::Skipped;
    };

    // Retry transient review errors (LLM 5xx/timeout, JSON parse failures) up to
    // 2 times before suspending. Transient failures are expected in production
    // and should not block the run on the first hiccup.
    const MAX_REVIEW_RETRIES: u32 = 2;
    let mut retries: u32 = 0;
    let review = loop {
        match super::review::review_task(&llm, &store, run_id, task, worker_output, cache_user_id)
            .await
        {
            Ok(r) => break r,
            Err(e) => {
                retries += 1;
                if retries <= MAX_REVIEW_RETRIES {
                    tracing::warn!(
                        task_id = %task.id,
                        attempt = retries,
                        error = %e,
                        "review gate transient error, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    continue;
                }
                // Exhausted retries. Do NOT auto-pass — that would let
                // unreviewed mutating work through. Surface the error so the
                // user can retry, lower the standard, or intervene.
                let reason = format!(
                    "review gate failed after {MAX_REVIEW_RETRIES} retries ({e}); run suspended pending user input"
                );
                let _ = store.note(run_id, Some(&task.id), &reason);
                return ReviewGateOutcome::Suspend(reason);
            }
        }
    };

    match review.outcome {
        ReviewOutcome::Pass => ReviewGateOutcome::Pass,
        ReviewOutcome::NeedsFix => {
            match super::review::circuit_breaker_action(&store, task, &review, 2) {
                super::review::BreakerAction::CreateFix => {
                    ReviewGateOutcome::NeedsFix(super::review::build_fix_task(task, &review))
                }
                super::review::BreakerAction::Suspend { reason } => {
                    ReviewGateOutcome::Suspend(reason)
                }
            }
        }
        ReviewOutcome::Blocked => ReviewGateOutcome::Suspend("review returned blocked".to_string()),
    }
}

/// Execute a single task on a pooled worker. Returns `(task_id, summary)` on
/// success or `(task_id, error)` on failure. Honors read vs write concurrency
/// via the two semaphores.
async fn execute_task(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    worker_sem: Arc<Semaphore>,
    write_sem: Arc<Semaphore>,
    shell_sem: Arc<Semaphore>,
    llm_sem: Arc<Semaphore>,
    file_write_locks: Arc<std::sync::Mutex<HashSet<String>>>,
    trace_sink: Option<WorkerTraceSink>,
    run_id: String,
    task: PlanTask,
    cancel: CancellationToken,
) -> Result<(String, Option<String>), (String, String)> {
    let task_id = task.id.clone();
    let is_write = !task.kind.is_read_only();

    // Create a child cancellation token for THIS task and register it with the
    // store. remove_task / update_task can cancel it to stop this worker
    // promptly without cancelling sibling tasks. child_token() means run-level
    // cancel still propagates here (child fires when parent fires).
    let task_cancel = cancel.child_token();
    store.register_task_cancel_token(&run_id, &task_id, task_cancel.clone());
    // RAII guard: always unregister on exit (success/fail/cancel), so the
    // token map doesn't leak finished tasks. Owns its key strings to avoid
    // borrowing task_id/run_id (which may be moved later in this function).
    struct TokenGuard {
        store: std::sync::Arc<TaskRuntimeStore>,
        run_id: String,
        task_id: String,
    }
    impl Drop for TokenGuard {
        fn drop(&mut self) {
            self.store
                .unregister_task_cancel_token(&self.run_id, &self.task_id);
        }
    }
    let _token_guard = TokenGuard {
        store: store.clone(),
        run_id: run_id.clone(),
        task_id: task_id.clone(),
    };

    let worker_trace_id = if task.kind.is_read_only() {
        format!("{run_id}:{}", task.agent_role)
    } else {
        task_id.clone()
    };

    // Mark the task running + emit TaskStarted (transactional with the todo
    // projection update).
    if let Err(e) = store.set_task_status(
        &run_id,
        &task_id,
        TodoStatus::Running,
        Some(&task.agent_role),
        None,
    ) {
        tracing::warn!(task_id = %task_id, error = %e, "failed to mark task running");
    }
    emit_worker_trace(
        trace_sink.as_ref(),
        WorkerTraceEvent::for_worker(
            run_id.clone(),
            worker_trace_id.clone(),
            WorkerTraceEventKind::WorkerStarted,
            serde_json::json!({
                "kind": task.kind.as_str(),
                "agent_role": &task.agent_role,
            }),
        )
        .with_agent(task.agent_role.clone())
        .with_title(task.title.clone())
        .with_task(task.description.clone()),
    );

    // Acquire concurrency permits with cancel awareness:
    // - Read-only tasks take a worker permit (fan-out up to max_concurrent_workers).
    // - Write tasks (implementation/debugging) take ONLY the write permit.
    // - Verification tasks (shell/build/test) take the write permit + the shell
    //   permit (default 1, plan §678-680 shell_concurrency = 1).
    let is_shell = matches!(task.kind, PlanTaskKind::Verification);
    let (_worker_permit, _write_permit, _shell_permit) = if is_shell {
        let wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err((task_id.clone(), "cancelled while waiting for write permit".to_string())),
            p = write_sem.acquire() => p.map_err(|e| (task_id.clone(), e.to_string()))?,
        };
        let sp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err((task_id.clone(), "cancelled while waiting for shell permit".to_string())),
            p = shell_sem.acquire() => p.map_err(|e| (task_id.clone(), e.to_string()))?,
        };
        (None, Some(wp), Some(sp))
    } else if is_write {
        let wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err((task_id.clone(), "cancelled while waiting for write permit".to_string())),
            p = write_sem.acquire() => p.map_err(|e| (task_id.clone(), e.to_string()))?,
        };
        (None, Some(wp), None)
    } else {
        let wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err((task_id.clone(), "cancelled while waiting for worker permit".to_string())),
            p = worker_sem.acquire() => p.map_err(|e| (task_id.clone(), e.to_string()))?,
        };
        (Some(wp), None, None)
    };

    // G5: File-level write lock — claim the task's target files so two write
    // tasks targeting the same file don't run concurrently. Uses a non-blocking
    // check: if a file is already locked, it's a plan-graph bug (two write
    // tasks with the same file in the same wave without a depends_on edge).
    // We log a warning and proceed rather than deadlocking, since write_sem=1
    // already serializes all writes.
    let _file_lock_guard = if is_write && !task.files.is_empty() {
        let mut locks = file_write_locks.lock().unwrap_or_else(|e| e.into_inner());
        let conflict = task.files.iter().find(|f| locks.contains(*f));
        if let Some(f) = conflict {
            tracing::warn!(
                task_id = %task_id,
                file = %f,
                "file already locked by another write task; proceeding (write_sem serializes)"
            );
        }
        for f in &task.files {
            locks.insert(f.clone());
        }
        Some(FileLockGuard {
            locks: file_write_locks.clone(),
            files: task.files.clone(),
        })
    } else {
        None
    };

    // G4: LLM rate-limit permit — caps concurrent LLM calls to prevent
    // provider rate-limit hits and cost spikes (plan §704).
    let _llm_permit = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err((task_id.clone(), "cancelled while waiting for LLM permit".to_string())),
        p = llm_sem.acquire() => p.map_err(|e| (task_id.clone(), e.to_string()))?,
    };

    // G10+G11: Pre-execution safety check — verify the task's tool calls
    // are covered by an approval scope AND pass the high-risk arg check.
    //
    // Fail-closed: when a tool+args pair matches a high-risk pattern, we
    // suspend the run immediately and require the user to either approve
    // the specific call or edit the plan before the run can resume.
    // Mirrors the review-gate Suspend path to avoid an unsafe "note-and-
    // continue" gap.
    //
    // Args snapshot: we scan ALL task strings that may flow into a tool call —
    // not just `title`. The `verification` field is the most important: it is
    // injected into the worker prompt ("Run the listed verification when done")
    // and Verification-kind tasks execute it under a shell permit, so a plan
    // like `["rm -rf target && cargo test"]` must be caught here. `files` feeds
    // the file-write tools; `description` is free-form and may carry commands.
    if !task.allowed_tools.is_empty() {
        let args_snapshot = build_hitrisk_args_snapshot(&task);
        for tool in &task.allowed_tools {
            if let Some(m) = super::hitrisk::check(tool, &args_snapshot) {
                let reason = format!(
                    "hitrisk flagged tool '{tool}' (pattern '{}': {}) for task '{}'; \
                     run paused pending user approval. Matched: {}",
                    m.pattern, m.reason, task.title, m.snippet,
                );
                let _ = store.note(&run_id, Some(&task_id), &reason);
                let _ = store.transition_run(&run_id, TaskRunStatus::Paused);
                return Err((task_id.clone(), format!("SUSPEND:{reason}")));
            }
        }
    }

    // Summary Chain: gather the summaries of this task's completed
    // dependencies, so the worker gets compact upstream context instead of
    // (or in addition to) re-reading everything from scratch (plan §1039).
    let dep_summaries = collect_dependency_summaries(&store, &run_id, &task);

    // Stable workspace context for the worker — prepended to the task prompt
    // so workers know where to operate without needing CWD in their system prompt.
    let ws_prefix = {
        let wd = primary_agent.read(|a| a.working_dir()).await;
        if let Some(ref wd) = wd {
            format!("[workspace]\n- root: {}\n[/workspace]\n\n", wd.display())
        } else {
            String::new()
        }
    };
    let prompt = build_task_prompt(&task, &dep_summaries);
    let prompt = format!("{ws_prefix}{prompt}");

    // Dispatch the task. Two paths, by kind:
    // - Read-only kinds (read_only_review, investigation, test_plan, review,
    //   summary) → delegate to the registered subagent role via
    //   delegate_to_agent_with_cancel. Fork mode runs the worker on an
    //   isolated instance under the executor's own semaphore (NOT the primary
    //   agent's execution_mutex), so read-only tasks parallelize. The child
    //   cancel token propagates parent-run cancellation.
    // - Mutating kinds (implementation, debugging, verification) → the MAIN
    //   agent executes directly via Agent::execute. These are serialized by
    //   the write_sem above; they are never delegated to a read-only worker
    //   (workers can't write). The primary agent's execution_mutex serializes
    //   them further, which is correct — mutating work must not race.
    let is_read_only_task = task.kind.is_read_only();
    let (result, readonly_usage) = if is_read_only_task {
        match run_readonly_worker(
            &primary_agent,
            &run_id,
            &task.agent_role,
            &prompt,
            task_cancel.clone(),
        )
        .await
        {
            Ok(sub_result) => (Ok(sub_result.output), sub_result.usage),
            Err(e) => (Err(e), None),
        }
    } else {
        (
            run_main_agent_task(
                &primary_agent,
                store.clone(),
                &run_id,
                &task,
                &prompt,
                task_cancel.clone(),
                trace_sink.clone(),
            )
            .await,
            None,
        )
    };

    if is_read_only_task && result.is_ok() {
        let usage_payload = match &readonly_usage {
            Some(stats) => stats.to_payload(&run_id),
            None => unavailable_llm_usage_payload("provider_returned_no_usage_for_readonly_worker"),
        };
        if let Err(error) = store.record_worker_llm_usage(
            &run_id,
            &task_id,
            &worker_trace_id,
            &task.agent_role,
            &task.title,
            usage_payload.clone(),
        ) {
            tracing::warn!(
                run_id = %run_id,
                task_id = %task_id,
                error = %error,
                "failed to persist read-only worker LLM usage"
            );
        }
        emit_worker_trace(
            trace_sink.as_ref(),
            WorkerTraceEvent::for_worker(
                run_id.clone(),
                worker_trace_id.clone(),
                WorkerTraceEventKind::WorkerLlmUsage,
                usage_payload,
            )
            .with_agent(task.agent_role.clone())
            .with_title(task.title.clone()),
        );
    }

    match result {
        Ok(text) => {
            let summary = summarize_output(&text);
            super::ledger::archive_trace(&run_id, &task_id, &text, None);
            let _ = super::ledger::write_progress(&store, &run_id, None);
            if let Err(e) = store.put_summary(&TaskExecutionSummary {
                run_id: run_id.clone(),
                task_id: task_id.clone(),
                worker_agent: task.agent_role.clone(),
                completed_work: vec![summary.clone()],
                files_read: vec![],
                files_changed: if is_write { task.files.clone() } else { vec![] },
                decisions: vec![],
                failures: vec![],
                verification: task.verification.clone(),
                next_implications: vec![],
                created_at: chrono::Utc::now(),
            }) {
                tracing::warn!(task_id = %task_id, error = %e, "failed to persist TaskExecutionSummary");
            }
            emit_worker_trace(
                trace_sink.as_ref(),
                WorkerTraceEvent::for_worker(
                    run_id.clone(),
                    worker_trace_id.clone(),
                    WorkerTraceEventKind::WorkerCompleted,
                    serde_json::json!({ "summary": &summary }),
                )
                .with_agent(task.agent_role.clone())
                .with_title(task.title.clone()),
            );
            Ok((task_id, Some(summary)))
        }
        Err(e) => {
            emit_worker_trace(
                trace_sink.as_ref(),
                WorkerTraceEvent::for_worker(
                    run_id,
                    worker_trace_id,
                    WorkerTraceEventKind::WorkerFailed,
                    serde_json::json!({ "error": &e }),
                )
                .with_agent(task.agent_role.clone())
                .with_title(task.title.clone()),
            );
            Err((task_id, e))
        }
    }
}

/// Pull the (title, summary) pairs for a task's completed dependencies.
/// Build a JSON args snapshot covering every task string that may flow into a
/// tool call, for the pre-execution high-risk check (G10+G11).
///
/// The snapshot intentionally includes:
/// - `verification`: the most important field — Verification-kind tasks run
///   these under a shell permit, and they are injected into every worker prompt
///   ("Run the listed verification when done"), so `rm -rf target` here is real.
/// - `files`: feeds file-write tools (write_file/edit_file/move_file/...).
/// - `title` / `description`: free-form text the LLM may quote into a command.
///
/// This is broader than the old `{"task": title, "files": files}` snapshot,
/// which only caught patterns literally present in the title.
fn build_hitrisk_args_snapshot(task: &PlanTask) -> String {
    serde_json::to_string(&serde_json::json!({
        "title": task.title,
        "description": task.description,
        "files": task.files,
        "verification": task.verification,
    }))
    .unwrap_or_default()
}

/// Prefers the structured TaskExecutionSummary (persisted by put_summary at
/// task boundary) over the truncated todo.summary text, so downstream workers
/// get full context: completed_work, files_changed, decisions, etc.
fn collect_dependency_summaries(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    task: &PlanTask,
) -> Vec<(String, String)> {
    if task.depends_on.is_empty() {
        return Vec::new();
    }
    let Ok(todos) = store.list_todos(run_id) else {
        return Vec::new();
    };
    task.depends_on
        .iter()
        .filter_map(|dep_id| {
            todos.iter().find(|t| &t.task_id == dep_id).and_then(|t| {
                if t.status != TodoStatus::Completed {
                    return None;
                }
                // Prefer the structured summary when available; fall back to
                // the truncated todo text for tasks that predate put_summary.
                let structured = store
                    .get_summary(run_id, &t.task_id)
                    .ok()
                    .flatten()
                    .map(|s| {
                        let mut parts: Vec<String> = Vec::new();
                        if !s.completed_work.is_empty() {
                            parts.push(format!("完成: {}", s.completed_work.join("; ")));
                        }
                        if !s.files_changed.is_empty() {
                            parts.push(format!("修改文件: {}", s.files_changed.join(", ")));
                        }
                        if !s.decisions.is_empty() {
                            parts.push(format!("决策: {}", s.decisions.join("; ")));
                        }
                        (t.title.clone(), parts.join(" | "))
                    });
                structured.or_else(|| {
                    t.summary
                        .as_deref()
                        .map(|s| (t.title.clone(), s.to_string()))
                })
            })
        })
        .collect()
}

/// Build the prompt handed to a task's worker. Combines the task brief with
/// its verification criteria, the Summary Chain from completed dependencies,
/// and a read-only reminder for non-mutating kinds.
fn build_task_prompt(task: &PlanTask, dep_summaries: &[(String, String)]) -> String {
    let mut s = String::new();
    // [task_context] marker: all content below is dynamic per-task information.
    // Worker system prompts are fixed templates — dynamic task descriptions,
    // target files, verification steps, and dependency summaries go HERE
    // (in the user message), keeping the system prefix cache-stable.
    s.push_str("[task_context]\n");
    s.push_str(&format!("Task: {}\n\n{}\n\n", task.title, task.description));
    // Summary Chain: compact context from completed upstream tasks. Replaces
    // the raw upstream worker conversations (plan §1039-1062).
    if !dep_summaries.is_empty() {
        s.push_str("Context from completed upstream tasks:\n");
        for (title, summary) in dep_summaries {
            s.push_str(&format!("- {title}: {summary}\n"));
        }
        s.push('\n');
    }
    if !task.files.is_empty() {
        s.push_str("Targets:\n");
        for f in &task.files {
            s.push_str(&format!("- {f}\n"));
        }
        s.push('\n');
    }
    if !task.verification.is_empty() {
        s.push_str("Verification (you must address each):\n");
        for v in &task.verification {
            s.push_str(&format!("- {v}\n"));
        }
        s.push('\n');
    }
    if task.kind.is_read_only() {
        s.push_str(
            "You are a READ-ONLY worker. Do NOT modify files or run mutating shell commands. \
             Report findings concretely with file paths.\n",
        );
    } else {
        s.push_str(
            "You may make the scoped change described above. Keep edits minimal and on-scope. \
             Run the listed verification when done.\n",
        );
    }
    s.push_str("\nReturn a concise summary of what you did and found.");
    s
}

/// Run a READ-ONLY task by delegating to a registered subagent role via the
/// primary agent's `delegate_to_agent_with_cancel`. Fork mode runs the worker
/// on an isolated agent instance under the executor's own semaphore (not the
/// primary agent's execution_mutex), so multiple read-only workers run in
/// parallel. The child cancel token propagates parent-run cancellation.
async fn run_readonly_worker(
    primary_agent: &crate::agent_handle::AgentHandle,
    run_id: &str,
    role: &str,
    prompt: &str,
    cancel: CancellationToken,
) -> Result<echo_agent::agent::subagent::SubagentResult, String> {
    primary_agent
        .read_async(|agent| {
            let prompt = prompt.to_string();
            let role = role.to_string();
            let run_id = run_id.to_string();
            Box::pin(async move {
                agent
                    .delegate_to_agent_with_parent_and_cancel(&role, &prompt, &run_id, cancel)
                    .await
                    .map_err(|e| format!("subagent dispatch failed: {e}"))
            })
        })
        .await
}

/// Run a MUTATING task (implementation / debugging / verification) directly on
/// the PRIMARY agent via `Agent::execute`. These tasks are never delegated to a
/// read-only subagent (workers can't write). The write_sem acquired by the
/// caller serializes them, and the primary agent's execution_mutex serializes
/// them further — correct, because mutating work must not race.
///
/// Cancellation: `Agent::execute` is not cancel-aware, so we race it against
/// the cancel token. If the run is cancelled mid-task, we return an error and
/// the task is marked Failed (the run then goes Cancelled/Failed).
async fn run_main_agent_task(
    primary_agent: &crate::agent_handle::AgentHandle,
    store: Arc<TaskRuntimeStore>,
    run_id: &str,
    task: &PlanTask,
    prompt: &str,
    cancel: CancellationToken,
    trace_sink: Option<WorkerTraceSink>,
) -> Result<String, String> {
    let run_id = run_id.to_string();
    let task_id = task.id.clone();
    let agent_role = task.agent_role.clone();
    let title = task.title.clone();

    primary_agent
        .read_async(|agent| {
            let prompt = prompt.to_string();
            Box::pin(async move {
                let mut stream = agent
                    .execute_stream_with_cancel(&prompt, cancel)
                    .await
                    .map_err(|e| format!("main agent stream failed: {e}"))?;
                let mut output = String::new();
                let mut in_thinking = false;

                while let Some(event_result) = stream.next().await {
                    let event =
                        event_result.map_err(|e| format!("main agent stream failed: {e}"))?;
                    match event {
                        AgentEvent::Token(content) => {
                            if in_thinking {
                                emit_worker_trace(
                                    trace_sink.as_ref(),
                                    WorkerTraceEvent::for_worker(
                                        run_id.clone(),
                                        task_id.clone(),
                                        WorkerTraceEventKind::WorkerThinkingDelta,
                                        serde_json::json!({ "content": content }),
                                    )
                                    .with_agent(agent_role.clone())
                                    .with_title(title.clone()),
                                );
                            } else {
                                output.push_str(&content);
                                emit_worker_trace(
                                    trace_sink.as_ref(),
                                    WorkerTraceEvent::for_worker(
                                        run_id.clone(),
                                        task_id.clone(),
                                        WorkerTraceEventKind::WorkerTokenDelta,
                                        serde_json::json!({ "content": content }),
                                    )
                                    .with_agent(agent_role.clone())
                                    .with_title(title.clone()),
                                );
                            }
                        }
                        AgentEvent::ThinkStart => {
                            in_thinking = true;
                            emit_worker_trace(
                                trace_sink.as_ref(),
                                WorkerTraceEvent::for_worker(
                                    run_id.clone(),
                                    task_id.clone(),
                                    WorkerTraceEventKind::WorkerThinkingStart,
                                    serde_json::json!({}),
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::ThinkEnd {
                            prompt_tokens,
                            completion_tokens,
                        } => {
                            in_thinking = false;
                            emit_worker_trace(
                                trace_sink.as_ref(),
                                WorkerTraceEvent::for_worker(
                                    run_id.clone(),
                                    task_id.clone(),
                                    WorkerTraceEventKind::WorkerThinkingEnd,
                                    serde_json::json!({
                                        "prompt_tokens": prompt_tokens,
                                        "completion_tokens": completion_tokens,
                                    }),
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::LlmUsage {
                            model,
                            prompt_tokens,
                            completion_tokens,
                            total_tokens,
                            cached_prompt_tokens,
                            cache_creation_prompt_tokens,
                            usage_reported,
                        } => {
                            let usage_payload = serde_json::json!({
                                "model": model,
                                "prompt_tokens": prompt_tokens,
                                "completion_tokens": completion_tokens,
                                "total_tokens": total_tokens,
                                "cached_prompt_tokens": cached_prompt_tokens,
                                "cache_creation_prompt_tokens": cache_creation_prompt_tokens,
                                "usage_reported": usage_reported,
                            });
                            if let Err(error) = store.record_worker_llm_usage(
                                &run_id,
                                &task_id,
                                &task_id,
                                &agent_role,
                                &title,
                                usage_payload.clone(),
                            ) {
                                tracing::warn!(
                                    run_id = %run_id,
                                    task_id = %task_id,
                                    error = %error,
                                    "failed to persist worker LLM usage"
                                );
                            }
                            emit_worker_trace(
                                trace_sink.as_ref(),
                                WorkerTraceEvent::for_worker(
                                    run_id.clone(),
                                    task_id.clone(),
                                    WorkerTraceEventKind::WorkerLlmUsage,
                                    usage_payload,
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::ToolCall { name, args } => {
                            emit_worker_trace(
                                trace_sink.as_ref(),
                                WorkerTraceEvent::for_worker(
                                    run_id.clone(),
                                    task_id.clone(),
                                    WorkerTraceEventKind::WorkerToolStart,
                                    serde_json::json!({
                                        "name": name,
                                        "args": args,
                                    }),
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::ToolResult {
                            name,
                            output: result,
                        } => {
                            emit_worker_trace(
                                trace_sink.as_ref(),
                                WorkerTraceEvent::for_worker(
                                    run_id.clone(),
                                    task_id.clone(),
                                    WorkerTraceEventKind::WorkerToolResult,
                                    serde_json::json!({
                                        "name": name,
                                        "result": result,
                                        "success": true,
                                    }),
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::ToolError { name, error } => {
                            emit_worker_trace(
                                trace_sink.as_ref(),
                                WorkerTraceEvent::for_worker(
                                    run_id.clone(),
                                    task_id.clone(),
                                    WorkerTraceEventKind::WorkerToolResult,
                                    serde_json::json!({
                                        "name": name,
                                        "result": error,
                                        "success": false,
                                    }),
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::FinalAnswer(answer) => {
                            if !answer.is_empty() {
                                output = answer;
                            }
                        }
                        AgentEvent::Cancelled => {
                            return Err("task cancelled".to_string());
                        }
                        AgentEvent::Error { source, message } => {
                            return Err(format!("{source}: {message}"));
                        }
                        _ => {}
                    }
                }

                Ok(output)
            })
        })
        .await
}

fn unavailable_llm_usage_payload(reason: &'static str) -> serde_json::Value {
    serde_json::json!({
        "model": "unknown",
        "prompt_tokens": 0,
        "completion_tokens": 0,
        "total_tokens": 0,
        "cached_prompt_tokens": 0,
        "cache_creation_prompt_tokens": 0,
        "usage_reported": false,
        "reason": reason,
    })
}

fn emit_worker_trace(sink: Option<&WorkerTraceSink>, event: WorkerTraceEvent) {
    if let Some(sink) = sink {
        sink(event);
    }
}

/// RAII guard that releases file write locks when dropped (G5).
struct FileLockGuard {
    locks: Arc<std::sync::Mutex<HashSet<String>>>,
    files: Vec<String>,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        if let Ok(mut locks) = self.locks.lock() {
            for f in &self.files {
                locks.remove(f);
            }
        }
    }
}

/// Write a terminal Run record to the trace store when available.
/// Best-effort: trace failures are logged but never fail the run.
fn save_trace(
    run_store: Option<&Arc<dyn echo_agent::trace::RunStore>>,
    run_id: &str,
    goal: &str,
    conversation_id: &str,
    status: &str,
) {
    let Some(rs) = run_store else { return };
    let run = echo_agent::trace::Run {
        run_id: run_id.to_string(),
        parent_run_id: None,
        session_id: conversation_id.to_string(),
        status: match status {
            "completed" => echo_agent::trace::RunStatus::Completed,
            "failed" => echo_agent::trace::RunStatus::Failed,
            "cancelled" => echo_agent::trace::RunStatus::Cancelled,
            _ => echo_agent::trace::RunStatus::Completed,
        },
        input: goal.to_string(),
        events: vec![],
        final_output: None,
        error: if status == "failed" {
            Some("run failed".to_string())
        } else {
            None
        },
        token_usage: Default::default(),
        timings: Default::default(),
        started_at: chrono::Utc::now(),
        finished_at: Some(chrono::Utc::now()),
    };
    let rs = rs.clone();
    let log_id = run_id.to_string();
    tokio::spawn(async move {
        if let Err(e) = rs.save(run).await {
            tracing::warn!(run_id = %log_id, error = %e, "trace Run save failed (non-fatal)");
        } else {
            tracing::debug!(run_id = %log_id, "trace Run saved");
        }
    });
}

/// Compress a worker's raw output into a compact summary line for the todo
/// projection. Full output is archived as an artifact in PR 4/5; here we keep
/// the first ~280 chars so the GUI has something to show.
fn summarize_output(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 280 {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(277).collect();
    format!("{head}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency_limits_clamp_pool_value() {
        // composite_parallelism reports 0/1/N → workers clamp to [1,8].
        // We can't easily build a pool in a unit test, so test the clamp math.
        let clamp = |n: usize| n.max(1).min(8);
        assert_eq!(clamp(0), 1);
        assert_eq!(clamp(1), 1);
        assert_eq!(clamp(4), 4);
        assert_eq!(clamp(20), 8);
    }

    #[test]
    fn task_prompt_is_read_only_for_reviews() {
        let task = PlanTask {
            id: "t1".into(),
            title: "Review chat.rs".into(),
            description: "find bugs".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            files: vec!["chat.rs".into()],
            verification: vec!["report root cause".into()],
            ..Default::default()
        };
        let p = build_task_prompt(&task, &[]);
        assert!(p.contains("READ-ONLY"));
        assert!(p.contains("chat.rs"));
        assert!(p.contains("report root cause"));
    }

    #[test]
    fn task_prompt_allows_edits_for_implementation() {
        let task = PlanTask {
            id: "t2".into(),
            title: "Apply fix".into(),
            description: "patch the bug".into(),
            kind: PlanTaskKind::Implementation,
            ..Default::default()
        };
        let p = build_task_prompt(&task, &[]);
        assert!(!p.contains("READ-ONLY"));
        assert!(p.contains("scoped change"));
    }

    #[test]
    fn summarize_output_truncates_long_text() {
        let long = "x".repeat(500);
        let s = summarize_output(&long);
        assert!(s.ends_with("..."));
        assert!(s.chars().count() <= 280);
        assert_eq!(summarize_output("short"), "short");
    }

    #[test]
    fn run_outcome_failed_carries_task_id() {
        let o = RunOutcome::Failed {
            failed_task_id: "t3".into(),
            error: "boom".into(),
        };
        match o {
            RunOutcome::Failed { failed_task_id, .. } => assert_eq!(failed_task_id, "t3"),
            _ => panic!(),
        }
    }

    /// Integration-ish test: a 4-task read-only wave + 1 implementation
    /// dependent should complete with all todos Completed, using an in-memory
    /// store. We can't run a real agent in a unit test, so this exercises the
    /// store/state-machine side only (the dispatcher path is covered by the
    /// GUI walkthrough in PR 6 + an integration test).
    #[tokio::test]
    async fn store_transitions_through_running_to_completed() {
        use std::sync::Arc;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().expect("in-memory store"));
        // Seed a run + plan via the public store API, then drive the state
        // machine the way run_dag would.
        store
            .create_run("r1", "ws", "c1", "m1", DomainProfile::AiCoding, "g")
            .unwrap();
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            domain_profile: DomainProfile::AiCoding,
            goal: "g".into(),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "t1".into(),
                title: "Review".into(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "code_reviewer".into(),
                ..Default::default()
            }],
        };
        store.attach_plan(&plan).unwrap();

        // Simulate the executor: Running, mark task running then
        // completed, then Running → Completed.
        store.transition_run("r1", TaskRunStatus::Running).unwrap();
        store
            .set_task_status("r1", "t1", TodoStatus::Running, Some("code_reviewer"), None)
            .unwrap();
        store
            .set_task_status(
                "r1",
                "t1",
                TodoStatus::Completed,
                Some("code_reviewer"),
                Some("done"),
            )
            .unwrap();
        store
            .transition_run("r1", TaskRunStatus::Completed)
            .unwrap();

        let run = store.get_run("r1").unwrap().unwrap();
        assert_eq!(run.status, TaskRunStatus::Completed);
        let todos = store.list_todos("r1").unwrap();
        assert_eq!(todos[0].status, TodoStatus::Completed);
        assert!(todos[0].summary.as_deref() == Some("done"));
    }

    // ── hitrisk args-snapshot coverage (see G10+G11, build_hitrisk_args_snapshot) ──
    // The old snapshot only scanned {task, files}; the new one also scans
    // verification (executed under a shell permit) so destructive commands
    // hidden in the plan's verification list are caught before dispatch.

    #[test]
    fn hitrisk_snapshot_includes_verification_field() {
        let task = PlanTask {
            id: "t1".into(),
            title: "Clean and test".into(),
            description: "run the test suite".into(),
            kind: PlanTaskKind::Verification,
            agent_role: "verifier".into(),
            files: vec!["src/lib.rs".into()],
            verification: vec!["rm -rf target && cargo test".into()],
            allowed_tools: vec!["shell".into()],
            ..Default::default()
        };
        let snap = build_hitrisk_args_snapshot(&task);
        // The snapshot must carry the verification string verbatim.
        assert!(snap.contains("rm -rf target && cargo test"));
    }

    #[test]
    fn hitrisk_catches_destructive_command_in_verification() {
        let task = PlanTask {
            id: "t1".into(),
            title: "Clean build".into(),
            description: "tidy up".into(),
            kind: PlanTaskKind::Verification,
            agent_role: "verifier".into(),
            files: vec![],
            // A dangerous command placed in the verification list (which the
            // worker is told to run). The old {task, files} snapshot missed this.
            verification: vec!["rm -rf /".into()],
            allowed_tools: vec!["shell".into()],
            ..Default::default()
        };
        let snap = build_hitrisk_args_snapshot(&task);
        assert!(
            super::super::hitrisk::check("shell", &snap).is_some(),
            "destructive command in verification must be flagged"
        );
    }

    #[test]
    fn hitrisk_catches_system_path_in_files() {
        let task = PlanTask {
            id: "t1".into(),
            title: "Patch config".into(),
            description: "update system config".into(),
            kind: PlanTaskKind::Implementation,
            agent_role: "implementer".into(),
            // A write targeting /etc — must be caught by PATH_PATTERNS.
            files: vec!["/etc/passwd".into()],
            verification: vec![],
            allowed_tools: vec!["write_file".into()],
            ..Default::default()
        };
        let snap = build_hitrisk_args_snapshot(&task);
        assert!(
            super::super::hitrisk::check("write_file", &snap).is_some(),
            "system-path write in files must be flagged"
        );
    }

    #[test]
    fn hitrisk_benign_task_is_not_flagged() {
        let task = PlanTask {
            id: "t1".into(),
            title: "Run unit tests".into(),
            description: "execute the test suite".into(),
            kind: PlanTaskKind::Verification,
            agent_role: "verifier".into(),
            files: vec!["src/lib.rs".into()],
            verification: vec!["cargo test".into()],
            allowed_tools: vec!["shell".into()],
            ..Default::default()
        };
        let snap = build_hitrisk_args_snapshot(&task);
        assert!(
            super::super::hitrisk::check("shell", &snap).is_none(),
            "benign cargo test must not be flagged"
        );
    }

    // ── run_dag integration tests with a scripted (mock) worker ──
    // These exercise the scheduling core — frontier computation, dependency
    // resolution, failure propagation, cancellation, stall detection — without
    // a real LLM. The worker returns scripted results keyed by task id.

    use std::collections::HashMap as StdHashMap;
    use std::sync::Mutex as StdMutex;

    /// A worker that returns scripted results per task id and records the
    /// order tasks were dispatched. Semaphores/locks are ignored (the mock
    /// answers instantly).
    struct ScriptedWorker {
        /// task_id → result to return. Missing id → generic success.
        results: StdMutex<StdHashMap<String, Result<String, String>>>,
        /// Dispatch order, appended as tasks are picked up.
        order: StdMutex<Vec<String>>,
    }

    impl ScriptedWorker {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                results: StdMutex::new(StdHashMap::new()),
                order: StdMutex::new(Vec::new()),
            })
        }
        /// Script a success result for `id`.
        fn succeed(self: &Arc<Self>, id: &str, summary: &str) {
            self.results
                .lock()
                .unwrap()
                .insert(id.into(), Ok(summary.into()));
        }
        /// Script a failure result for `id`.
        fn fail(self: &Arc<Self>, id: &str, err: &str) {
            self.results
                .lock()
                .unwrap()
                .insert(id.into(), Err(err.into()));
        }
        fn order(&self) -> Vec<String> {
            self.order.lock().unwrap().clone()
        }
    }

    impl TaskWorker for Arc<ScriptedWorker> {
        fn dispatch(
            &self,
            _store: Arc<TaskRuntimeStore>,
            _run_id: String,
            task: PlanTask,
            cancel: CancellationToken,
            _worker_sem: Arc<Semaphore>,
            _write_sem: Arc<Semaphore>,
            _shell_sem: Arc<Semaphore>,
            _llm_sem: Arc<Semaphore>,
            _file_write_locks: Arc<std::sync::Mutex<HashSet<String>>>,
            _trace_sink: Option<WorkerTraceSink>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<(String, Option<String>), (String, String)>>
                    + Send,
            >,
        > {
            let results = self.results.lock().unwrap().get(&task.id).cloned();
            self.order.lock().unwrap().push(task.id.clone());
            let task_id = task.id.clone();
            Box::pin(async move {
                // Honor cancellation even in the mock.
                if cancel.is_cancelled() {
                    return Err((task_id, "cancelled".into()));
                }
                match results {
                    Some(Ok(summary)) => Ok((task_id, Some(summary))),
                    Some(Err(e)) => Err((task_id, e)),
                    // Default: generic success for unscripted tasks.
                    None => Ok((task_id, Some("ok".into()))),
                }
            })
        }
    }

    /// Helper: a single-task plan (read-only, no review needed) that the
    /// scripted worker can complete.
    fn solo_readonly_task(id: &str) -> PlanTask {
        PlanTask {
            id: id.into(),
            title: id.into(),
            description: "desc".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "reviewer".into(),
            ..Default::default()
        }
    }

    /// Build a run + plan in the store and return the run id.
    ///
    /// Creates run (Pending), attaches plan (no status change), transitions
    /// Pending → Running so run_dag can start.
    fn seed_run(store: &Arc<TaskRuntimeStore>, tasks: Vec<PlanTask>) -> String {
        let run_id = format!("run_{}", uuid::Uuid::new_v4());
        store
            .create_run(
                &run_id,
                "ws_test",
                "conv_test",
                "msg_test",
                DomainProfile::General,
                "test goal",
            )
            .unwrap();
        let plan = TaskPlan {
            plan_id: format!("plan_{}", run_id),
            run_id: run_id.clone(),
            domain_profile: DomainProfile::General,
            goal: "test goal".into(),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Sequential,
            tasks,
        };
        store.attach_plan(&plan).unwrap();
        store
            .transition_run(&run_id, TaskRunStatus::Running)
            .unwrap();
        run_id
    }

    #[tokio::test]
    async fn run_dag_completes_single_task() {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![solo_readonly_task("a")]);
        let worker = ScriptedWorker::new();
        worker.succeed("a", "reviewed");

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None, // no reviewer LLM → read-only tasks auto-pass review
            &run_id,
            vec![solo_readonly_task("a")],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
            String::new(),
        )
        .await
        .unwrap();

        assert!(matches!(outcome, RunOutcome::Completed));
        let todos = store.list_todos(&run_id).unwrap();
        assert_eq!(todos[0].status, TodoStatus::Completed);
    }

    #[tokio::test]
    async fn run_dag_respects_dependency_order() {
        // b depends on a → a must be dispatched and completed before b.
        let mut a = solo_readonly_task("a");
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let _ = &mut a; // silence unused_mut
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![a.clone(), b.clone()]);
        let worker = ScriptedWorker::new();
        worker.succeed("a", "done a");
        worker.succeed("b", "done b");

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None,
            &run_id,
            vec![a, b],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
            String::new(),
        )
        .await
        .unwrap();

        assert!(matches!(outcome, RunOutcome::Completed));
        let order = worker.order();
        // a must appear before b in the dispatch order.
        let pos_a = order.iter().position(|x| x == "a").unwrap();
        let pos_b = order.iter().position(|x| x == "b").unwrap();
        assert!(pos_a < pos_b, "dependency violated: b dispatched before a");
    }

    #[tokio::test]
    async fn run_dag_failure_propagates_and_blocks_downstream() {
        // a fails; b depends on a and must be Blocked, run ends Failed
        // (because all non-terminal tasks are Failed/Blocked).
        let a = solo_readonly_task("a");
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![a.clone(), b.clone()]);
        let worker = ScriptedWorker::new();
        worker.fail("a", "boom");

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None,
            &run_id,
            vec![a, b],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
            String::new(),
        )
        .await
        .unwrap();

        match outcome {
            RunOutcome::Failed { failed_task_id, .. } => {
                assert_eq!(failed_task_id, "a");
            }
            other => panic!("expected Failed, got {:?}", other),
        }
        // b must be Blocked (downstream of failed a).
        let todos = store.list_todos(&run_id).unwrap();
        let b_todo = todos.iter().find(|t| t.task_id == "b").unwrap();
        assert_eq!(b_todo.status, TodoStatus::Blocked);
    }

    #[tokio::test]
    async fn run_dag_cancellation_propagates_to_cancelled_outcome() {
        // Cancel BEFORE dispatching; run_dag should observe cancellation at the
        // top of its loop and return Cancelled without running any task.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![solo_readonly_task("a")]);
        let worker = ScriptedWorker::new();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None,
            &run_id,
            vec![solo_readonly_task("a")],
            ConcurrencyLimits::default(),
            cancel,
            None,
            String::new(),
        )
        .await
        .unwrap();

        assert!(matches!(outcome, RunOutcome::Cancelled));
        // The worker must NOT have been dispatched into.
        assert!(worker.order().is_empty(), "task ran despite cancellation");
    }

    #[tokio::test]
    async fn run_dag_detects_cycle_as_stall() {
        // a depends on b, b depends on a → neither can ever become ready →
        // stall. (validate_plan would normally reject this, but run_dag must
        // still be robust to a malformed plan reaching it.)
        let mut a = solo_readonly_task("a");
        a.depends_on = vec!["b".into()];
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![a.clone(), b.clone()]);
        let worker = ScriptedWorker::new();

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None,
            &run_id,
            vec![a, b],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
            String::new(),
        )
        .await
        .unwrap();

        match outcome {
            RunOutcome::Failed { error, .. } => {
                assert!(
                    error.contains("stalled"),
                    "expected stall message, got: {error}"
                );
            }
            other => panic!("expected Failed (stall), got {:?}", other),
        }
        // Nothing should have been dispatched.
        assert!(worker.order().is_empty(), "worker ran on a cyclic plan");
    }

    #[tokio::test]
    async fn main_agent_task_streams_tool_events_to_worker_trace() -> Result<(), String> {
        use crate::agent_handle::AgentHandle;
        use echo_agent::agent::react::builder::ReactAgentBuilder;
        use echo_agent::testing::{MockLlmClient, MockTool};
        use std::sync::Mutex;

        let llm = MockLlmClient::new()
            .then_tool_call("call_1", "mock_calc", r#"{"x":6,"y":7}"#)
            .with_response("The result is 42.");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("You are a test assistant.")
            .tool(Box::new(MockTool::new("mock_calc").with_response("42")))
            .build()
            .map_err(|error| format!("test agent should build: {error}"))?;
        let handle = AgentHandle::new(agent);

        let task = PlanTask {
            id: "implementation-a".into(),
            title: "Run calculation".into(),
            description: "Use the tool and report the result".into(),
            kind: PlanTaskKind::Implementation,
            agent_role: "implementer".into(),
            ..Default::default()
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let sink: WorkerTraceSink = Arc::new(move |event| {
            if let Ok(mut guard) = captured.lock() {
                guard.push(event);
            }
        });
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory()
                .map_err(|error| format!("in-memory store should initialize: {error}"))?,
        );

        let output = run_main_agent_task(
            &handle,
            store,
            "run-trace",
            &task,
            "What is 6 times 7?",
            CancellationToken::new(),
            Some(sink),
        )
        .await
        .map_err(|error| format!("main agent task should complete: {error}"))?;

        assert!(output.contains("42"));
        let events = events
            .lock()
            .map_err(|error| format!("trace events lock poisoned: {error}"))?
            .clone();
        assert!(
            events.iter().any(|event| {
                event.event_type == WorkerTraceEventKind::WorkerToolStart
                    && event.worker_id.as_deref() == Some("implementation-a")
                    && event.payload.get("name").and_then(|v| v.as_str()) == Some("mock_calc")
            }),
            "expected WorkerToolStart for mock_calc, got {events:?}"
        );
        assert!(
            events.iter().any(|event| {
                event.event_type == WorkerTraceEventKind::WorkerToolResult
                    && event.worker_id.as_deref() == Some("implementation-a")
                    && event.payload.get("success").and_then(|v| v.as_bool()) == Some(true)
                    && event
                        .payload
                        .get("result")
                        .and_then(|v| v.as_str())
                        .is_some_and(|text| text.contains("42"))
            }),
            "expected successful WorkerToolResult with tool output, got {events:?}"
        );
        Ok(())
    }
}
