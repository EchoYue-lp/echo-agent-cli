//! Canonical EKO lifecycle for one finite primary-Agent RunTurn.
//!
//! Framework `AgentTurnDriver` owns the stream and typed terminal. This module
//! persists that terminal once, then applies EKO continuation, provider retry,
//! budget, cancellation, and Goal settlement policy for every caller.

use std::sync::Arc;

use echo_agent::runtime::TurnOutcome;

use super::executor::{ExecEvent, ExecSink, TaskRuntimeBlockingAdapter};
use super::store::{ProviderRetryDisposition, RunTurnCompletion, TaskRuntimeStore};
use super::types::{RunPauseReason, RunTurnStatus, RuntimeEventKind, TaskRunStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunTurnDecision {
    Stop,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreDriverRejection {
    Shutdown,
    Cancelled,
    Admission,
}

pub(crate) struct RunTurnTerminal<'a> {
    pub turn_id: &'a str,
    pub terminal: &'a TurnOutcome,
    pub elapsed_seconds: u64,
    pub final_message_id: Option<&'a str>,
}

pub(crate) struct PersistedRunTurn {
    continuation: super::types::RunContinuationState,
    error_fingerprint: Option<String>,
}

pub(crate) async fn persist_run_turn_terminal(
    blocking: &TaskRuntimeBlockingAdapter,
    run_id: &str,
    terminal: &RunTurnTerminal<'_>,
) -> Result<PersistedRunTurn, String> {
    let (status, error_fingerprint) = match terminal.terminal {
        TurnOutcome::Completed => (RunTurnStatus::Ended, None),
        TurnOutcome::Cancelled => (RunTurnStatus::Cancelled, Some("cancelled".to_string())),
        TurnOutcome::Failed(failure) => (
            RunTurnStatus::Failed,
            Some(agent_failure_fingerprint(failure)),
        ),
    };
    let agent_failure = match terminal.terminal {
        TurnOutcome::Failed(failure) => Some(failure.clone()),
        TurnOutcome::Completed | TurnOutcome::Cancelled => None,
    };
    let run_id = run_id.to_string();
    let turn_id = terminal.turn_id.to_string();
    let elapsed_seconds = terminal.elapsed_seconds;
    let final_message_id = terminal.final_message_id.map(str::to_string);
    let persisted_fingerprint = error_fingerprint.clone();
    let continuation = blocking
        .run_store("persist RunTurn terminal", move |store| {
            store.finish_run_turn_with_agent_failure(
                &run_id,
                RunTurnCompletion {
                    turn_id: &turn_id,
                    status,
                    elapsed_seconds,
                    final_message_id: final_message_id.as_deref(),
                    error_fingerprint: persisted_fingerprint.as_deref(),
                },
                agent_failure.as_ref(),
            )
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(PersistedRunTurn {
        continuation,
        error_fingerprint,
    })
}

fn decide_after_persisted_run_turn_sync(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    terminal: &RunTurnTerminal<'_>,
    persisted: PersistedRunTurn,
    trace_sink: Option<&ExecSink>,
) -> Result<RunTurnDecision, String> {
    let run = store
        .get_run(run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("TaskRun disappeared while finishing RunTurn: {run_id}"))?;
    if persisted.continuation.enabled && !store.is_run_driver_admission_open() {
        if run.status == TaskRunStatus::Running {
            let _paused = store
                .request_pause_with_reason(
                    run_id,
                    RunPauseReason::BootRecovery,
                    Some("application shutdown interrupted an active continuation turn"),
                )
                .map_err(|error| error.to_string())?;
        }
        return Ok(RunTurnDecision::Stop);
    }
    if let TurnOutcome::Failed(failure) = terminal.terminal
        && failure.retryable
        && failure.category == echo_agent::error::AgentFailureCategory::Llm
    {
        let fingerprint = persisted
            .error_fingerprint
            .as_deref()
            .ok_or_else(|| "typed provider failure lost its stable fingerprint".to_string())?;
        return store
            .schedule_provider_retry(run_id, fingerprint)
            .map(|disposition| match disposition {
                ProviderRetryDisposition::Scheduled(_) => RunTurnDecision::Continue,
                ProviderRetryDisposition::Exhausted(_) => RunTurnDecision::Stop,
            })
            .map_err(|error| error.to_string());
    }
    if run.status == TaskRunStatus::Completed && matches!(terminal.terminal, TurnOutcome::Completed)
    {
        if let Some(trace_sink) = trace_sink {
            trace_sink(ExecEvent::run(
                run.workspace_id,
                run.conversation_id,
                run_id.to_string(),
                RuntimeEventKind::RunCompleted,
                serde_json::json!({ "status": "completed" }),
            ));
        }
        return Ok(RunTurnDecision::Stop);
    }
    if run.status != TaskRunStatus::Running {
        return Ok(RunTurnDecision::Stop);
    }

    match terminal.terminal {
        TurnOutcome::Cancelled => {
            store
                .transition_run(run_id, TaskRunStatus::Cancelled)
                .map_err(|error| error.to_string())?;
            store
                .stop_owned_command_cells(run_id)
                .map_err(|error| error.to_string())?;
            if let Some(trace_sink) = trace_sink {
                trace_sink(ExecEvent::run(
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({ "status": "cancelled", "mode": "task" }),
                ));
            }
            return Ok(RunTurnDecision::Stop);
        }
        TurnOutcome::Failed(failure) => {
            if failure.retryable {
                let _paused = store
                    .request_pause_with_reason(
                        run_id,
                        RunPauseReason::NeedsInput,
                        Some(&failure.message),
                    )
                    .map_err(|error| error.to_string())?;
            } else {
                store
                    .transition_run(run_id, TaskRunStatus::Failed)
                    .map_err(|error| error.to_string())?;
            }
            return Ok(RunTurnDecision::Stop);
        }
        TurnOutcome::Completed => {}
    }

    if persisted
        .continuation
        .token_budget
        .is_some_and(|budget| persisted.continuation.tokens_used >= budget)
    {
        let _paused = store
            .request_pause_with_reason(
                run_id,
                RunPauseReason::TokenBudget,
                Some("the configured long-horizon token budget was exhausted"),
            )
            .map_err(|error| error.to_string())?;
        return Ok(RunTurnDecision::Stop);
    }
    if persisted
        .continuation
        .time_budget_seconds
        .is_some_and(|budget| persisted.continuation.time_used_seconds >= budget)
    {
        let _paused = store
            .request_pause_with_reason(
                run_id,
                RunPauseReason::TimeBudget,
                Some("the configured long-horizon time budget was exhausted"),
            )
            .map_err(|error| error.to_string())?;
        return Ok(RunTurnDecision::Stop);
    }
    let active_cells = store
        .defer_continuation_for_active_cells(run_id)
        .map_err(|error| error.to_string())?;
    if active_cells > 0 {
        tracing::info!(
            run_id,
            active_cells,
            "long-horizon continuation deferred until background cells settle"
        );
        return Ok(RunTurnDecision::Continue);
    }
    if persisted
        .continuation
        .blocker_audit
        .as_ref()
        .is_some_and(|audit| audit.consecutive_turns >= 3)
    {
        let _paused = store
            .request_pause_with_reason(
                run_id,
                RunPauseReason::RepeatedBlocker,
                Some("three consecutive RunTurns ended without TaskRuntime progress"),
            )
            .map_err(|error| error.to_string())?;
        return Ok(RunTurnDecision::Stop);
    }
    if persisted.continuation.enabled && !persisted.continuation.deferred {
        return Ok(RunTurnDecision::Continue);
    }

    let reason = match store.get_plan(run_id) {
        Ok(Some(_)) => "Task mode turn ended before task_execute reached a terminal result",
        _ => "Task mode turn ended without creating a formal plan",
    };
    store
        .note(run_id, None, reason)
        .map_err(|error| error.to_string())?;
    store
        .transition_run(run_id, TaskRunStatus::Failed)
        .map_err(|error| error.to_string())?;
    if let Some(trace_sink) = trace_sink {
        trace_sink(ExecEvent::run(
            run.workspace_id,
            run.conversation_id,
            run_id.to_string(),
            RuntimeEventKind::RunFailed,
            serde_json::json!({ "error": reason, "mode": "task" }),
        ));
    }
    Ok(RunTurnDecision::Stop)
}

pub(crate) async fn decide_after_persisted_run_turn(
    blocking: &TaskRuntimeBlockingAdapter,
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    terminal: &RunTurnTerminal<'_>,
    persisted: PersistedRunTurn,
    trace_sink: Option<&ExecSink>,
) -> Result<RunTurnDecision, String> {
    let run_id = run_id.to_string();
    let owned_terminal = terminal.terminal.clone();
    let owned_turn_id = terminal.turn_id.to_string();
    let elapsed_seconds = terminal.elapsed_seconds;
    let owned_final_message_id = terminal.final_message_id.map(str::to_string);
    let trace_sink = trace_sink.cloned();
    let store = Arc::clone(store);
    blocking
        .run_owned("decide after persisted RunTurn", move || {
            decide_after_persisted_run_turn_sync(
                &store,
                &run_id,
                &RunTurnTerminal {
                    turn_id: &owned_turn_id,
                    terminal: &owned_terminal,
                    elapsed_seconds,
                    final_message_id: owned_final_message_id.as_deref(),
                },
                persisted,
                trace_sink.as_ref(),
            )
            .map_err(super::store::StoreError::InvalidPlan)
        })
        .await
        .map_err(|error| error.to_string())
}

pub(crate) async fn finalize_run_turn(
    blocking: &TaskRuntimeBlockingAdapter,
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    terminal: &RunTurnTerminal<'_>,
    trace_sink: Option<&ExecSink>,
) -> Result<RunTurnDecision, String> {
    let persisted = persist_run_turn_terminal(blocking, run_id, terminal).await?;
    decide_after_persisted_run_turn(blocking, store, run_id, terminal, persisted, trace_sink).await
}

pub(crate) async fn reject_before_driver_start(
    blocking: &TaskRuntimeBlockingAdapter,
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    turn_id: &str,
    detail: &str,
    rejection: PreDriverRejection,
) -> Result<(), String> {
    let terminal = match rejection {
        PreDriverRejection::Cancelled => TurnOutcome::Cancelled,
        PreDriverRejection::Shutdown => TurnOutcome::Failed(
            echo_agent::error::AgentFailure::message("runtime_shutdown", detail),
        ),
        PreDriverRejection::Admission => TurnOutcome::Failed(
            echo_agent::error::AgentFailure::message("continuation_admission", detail),
        ),
    };
    persist_run_turn_terminal(
        blocking,
        run_id,
        &RunTurnTerminal {
            turn_id,
            terminal: &terminal,
            elapsed_seconds: 0,
            final_message_id: None,
        },
    )
    .await?;

    let run_id = run_id.to_string();
    let cleanup_run_id = run_id.clone();
    let detail = detail.to_string();
    blocking
        .run_store("settle pre-driver rejection", move |store| {
            let run_is_running = store
                .get_run(&run_id)?
                .is_some_and(|run| run.status == TaskRunStatus::Running);
            if !run_is_running {
                return Ok(());
            }
            match rejection {
                PreDriverRejection::Shutdown => store
                    .request_pause_with_reason(
                        &run_id,
                        RunPauseReason::BootRecovery,
                        Some("application shutdown interrupted pre-driver admission"),
                    )
                    .map(|_| ()),
                PreDriverRejection::Cancelled => {
                    store.transition_run(&run_id, TaskRunStatus::Cancelled)?;
                    store.stop_owned_command_cells(&run_id).map(|_| ())
                }
                PreDriverRejection::Admission => store
                    .request_pause_with_reason(&run_id, RunPauseReason::NeedsInput, Some(&detail))
                    .map(|_| ()),
            }
        })
        .await
        .map_err(|error| error.to_string())?;
    if rejection == PreDriverRejection::Cancelled {
        super::continuation::clear_launcher(store, &cleanup_run_id);
    }
    Ok(())
}

pub(crate) fn agent_failure_fingerprint(failure: &echo_agent::error::AgentFailure) -> String {
    let typed_identity = format!(
        "{:?}|{:?}|{}|{}",
        failure.category,
        failure.terminal_kind,
        failure.code,
        failure
            .http_status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "none".to_string())
    );
    super::task_goal_sha256(&typed_identity)
}
