//! Tauri IPC commands for the TaskRuntime.
//!
//! Read-only query commands, mutations for creating/managing task runs,
//! subagent execution, and route feedback learning.

use crate::tauri::commands::chat::TauriExecutionProjector;
use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

use echo_agent_app_core::tasks::task_runtime::types::*;
use std::sync::Arc;

// ── Helper: borrow the store or error ────────────────────────────────────

fn store(
    state: &tauri::State<'_, TauriState>,
) -> Result<std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>, IpcError> {
    state
        .app_state
        .tasks
        .runtime
        .clone()
        .ok_or_else(|| IpcError::Internal("TaskRuntime store not initialized".to_string()))
}

// ── Commands ─────────────────────────────────────────────────────────────

/// Fetch a single run by id.
#[tauri::command]
pub async fn get_task_run(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<Option<TaskRun>, IpcError> {
    store(&state)?.get_run(&run_id).map_err(internal)
}

/// Latest run for a conversation — binds a chat thread to its runtime run.
#[tauri::command]
pub async fn latest_task_run_for_conversation(
    state: tauri::State<'_, TauriState>,
    conversation_id: String,
) -> Result<Option<TaskRun>, IpcError> {
    store(&state)?
        .latest_run_for_conversation(&conversation_id)
        .map_err(internal)
}

/// All runs in any of the given statuses. Pass `None` or an empty list to
/// list every run (most recent first).
#[tauri::command]
pub async fn list_task_runs(
    state: tauri::State<'_, TauriState>,
    statuses: Option<Vec<String>>,
) -> Result<Vec<TaskRun>, IpcError> {
    let s = &store(&state)?;
    let parsed: Vec<TaskRunStatus> = statuses
        .unwrap_or_default()
        .iter()
        .filter_map(|s| TaskRunStatus::from_str(s))
        .collect();
    // Empty filter → list all. We do this by querying every known status,
    // which keeps the SQL path uniform (single `status IN (...)` query).
    let query: Vec<TaskRunStatus> = if parsed.is_empty() {
        vec![
            TaskRunStatus::Pending,
            TaskRunStatus::Running,
            TaskRunStatus::Paused,
            TaskRunStatus::Cancelled,
            TaskRunStatus::Failed,
            TaskRunStatus::Completed,
        ]
    } else {
        parsed
    };
    s.list_runs_in(&query).map_err(internal)
}

/// The structured plan attached to a run, or `None` if not yet generated.
#[tauri::command]
pub async fn get_task_plan(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<Option<TaskPlan>, IpcError> {
    store(&state)?.get_plan(&run_id).map_err(internal)
}

/// Todo projection for a run — what the right-rail Todo panel renders from.
#[tauri::command]
pub async fn list_task_todos(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<Vec<TodoItem>, IpcError> {
    store(&state)?.list_todos(&run_id).map_err(internal)
}

/// Events since `since_seq` (polling-style incremental event feed).
/// The GUI tracks the highest `seq` it has seen and polls with that value.
/// `since_seq` is a string because RuntimeTaskEvent.seq is serialized as a
/// string over IPC (i64 precision safety); we parse it back to i64 here.
#[tauri::command]
pub async fn list_task_events(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    since_seq: Option<String>,
) -> Result<Vec<RuntimeTaskEvent>, IpcError> {
    let since = since_seq
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    store(&state)?.list_events(&run_id, since).map_err(internal)
}

/// Artifacts produced by a run (files, reports, charts, traces).
#[tauri::command]
pub async fn list_task_artifacts(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<Vec<Artifact>, IpcError> {
    store(&state)?.list_artifacts(&run_id).map_err(internal)
}

/// Reviews recorded against a task within a run (scoped to run_id + task_id
/// so task-id collisions across runs don't bleed history).
#[tauri::command]
pub async fn list_task_reviews(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    task_id: String,
) -> Result<Vec<ReviewResult>, IpcError> {
    store(&state)?
        .list_reviews(&run_id, &task_id)
        .map_err(internal)
}

/// The execution summary a subagent produced for a task — used by the Summary
/// Chain (downstream subagents consume this instead of raw chat history).
#[tauri::command]
pub async fn get_task_summary(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    task_id: String,
) -> Result<Option<TaskExecutionSummary>, IpcError> {
    store(&state)?
        .get_summary(&run_id, &task_id)
        .map_err(internal)
}

/// Recovery barriers created when a mutating subagent/tool was interrupted
/// between its durable start and terminal boundaries.
#[tauri::command]
pub async fn list_recovery_blockers(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<Vec<RecoveryBlocker>, IpcError> {
    store(&state)?
        .list_recovery_blockers(&run_id)
        .map_err(internal)
}

/// Resolve an indeterminate side effect after the user inspected the
/// workspace. Supported decisions are `retry` and `skip`.
#[tauri::command]
pub async fn resolve_recovery_task(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    task_id: String,
    decision: String,
) -> Result<(), IpcError> {
    let decision = match decision.as_str() {
        "retry" => RecoveryDecision::Retry,
        "skip" => RecoveryDecision::Skip,
        _ => {
            return Err(IpcError::Validation(
                "recovery decision must be 'retry' or 'skip'".to_string(),
            ));
        }
    };
    store(&state)?
        .resolve_recovery_task(&run_id, &task_id, decision)
        .map_err(internal)
}

// ── Error mapping ────────────────────────────────────────────────────────

fn internal<E: std::fmt::Display>(e: E) -> IpcError {
    IpcError::Internal(e.to_string())
}

// ── Router toggle ────────────────────────────────────────────────────────

/// Set the manual interaction mode: 0=Auto, 1=Chat, 2=Task.
#[tauri::command]
pub async fn set_interaction_mode(
    state: tauri::State<'_, TauriState>,
    mode: u8,
) -> Result<u8, IpcError> {
    state
        .app_state
        .tasks
        .interaction_mode
        .store(mode, std::sync::atomic::Ordering::Relaxed);
    tracing::info!(mode, "interaction mode set");
    Ok(mode)
}

/// Get the current interaction mode.
#[tauri::command]
pub async fn get_interaction_mode(state: tauri::State<'_, TauriState>) -> Result<u8, IpcError> {
    Ok(state
        .app_state
        .tasks
        .interaction_mode
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Resume a paused run. Transitions `Paused → Running` and re-launches the
/// executor, which re-reads the plan from the store and skips already-completed
/// tasks.
#[tauri::command]
pub async fn resume_task_run(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = store(&state)?;
    if store.get_plan(&run_id).map_err(internal)?.is_none() {
        return Err(IpcError::Validation(format!(
            "run {run_id} has no persisted plan to resume"
        )));
    }

    let primary_agent = state.app_state.connection.primary_agent();
    let store_for_task = store.clone();
    let primary_agent_for_task = primary_agent.clone();
    let run_store_for_task = primary_agent.read(|a| a.run_store().cloned()).await;
    // (stage4 P4.1) cache_user_id read from single source by execute_run/review
    // internally — only the reviewer_llm is needed here.
    let reviewer_llm = primary_agent.read(|a| a.llm_client().cloned()).await;
    let layer_manager = state
        .app_state
        .review_integration
        .as_ref()
        .map(|ri| std::sync::Arc::new(ri.create_layer_manager()));
    let cancel = echo_agent::agent::CancellationToken::new();
    let cancel_registration = store
        .register_run_cancellation(&run_id, cancel.clone())
        .map_err(internal)?;
    store.resume_task_run(&run_id).map_err(internal)?;
    tracing::info!(run_id = %run_id, "task run resumed -> Running");
    let execution_projector = Arc::new(TauriExecutionProjector::new(
        app,
        state.app_state.storage.tool_executions.clone(),
        Some(store.clone()),
    ));
    let trace_sink: echo_agent_app_core::tasks::task_runtime::ExecSink = Arc::new(move |ev| {
        execution_projector.emit(ev);
    });
    let run_id_for_task = run_id.clone();
    // Wire the task lifecycle hook bridge (P0-5) from the primary agent so the
    // resumed GUI run also fires TaskCreated/Completed/Timeout/Cancelled.
    let gui_hook_bridge = primary_agent
        .read(|a| a.create_task_hook_bridge().bridge().clone())
        .await;
    let gui_subagent_bridge = primary_agent
        .read(|a| std::sync::Arc::new(a.create_subagent_hook_bridge()))
        .await;

    tokio::spawn(async move {
        let _cancel_registration = cancel_registration;
        let outcome = echo_agent_app_core::tasks::task_runtime::execute_run(
            store_for_task.clone(),
            Some(primary_agent_for_task),
            reviewer_llm,
            layer_manager,
            run_store_for_task,
            Some(trace_sink),
            &run_id_for_task,
            cancel,
            // GUI resume keeps the interactive asynchronous memory projection.
            echo_agent_app_core::tasks::task_runtime::MemoryPolicy::FireAndForget,
            Some(gui_hook_bridge),
            Some(gui_subagent_bridge),
        )
        .await;
        match outcome {
            Ok(echo_agent_app_core::tasks::task_runtime::RunOutcome::Completed) => {
                tracing::info!(run_id = %run_id_for_task, "resumed run completed");
            }
            Ok(other) => {
                tracing::warn!(run_id = %run_id_for_task, ?other, "resumed run ended non-completed");
            }
            Err(e) => {
                tracing::error!(run_id = %run_id_for_task, error = %e, "resumed run executor error");
            }
        }
    });

    Ok(serde_json::json!({
        "kind": "resumed",
        "run_id": run_id,
    }))
}

/// Explicitly retry a Blocked (or Failed) task in a Paused (or Failed) run.
///
/// This is the only sanctioned way to re-run a task whose previous attempt
/// completed but failed acceptance/review: the executor no longer
/// auto-redispatches on acceptance failure (M7). Bumps `retry_count`,
/// resets the task to Pending, transitions the run to Running, and spawns
/// the executor. Title and description are preserved; the next attempt
/// gets a fresh `execution_id = "{run_id}:{task_id}:{plan_revision}:{attempt}"`.
///
/// Honors the `max_retries` budget: returns a validation error when
/// `retry_count >= max_retries`.
#[tauri::command]
pub async fn retry_blocked_task(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    run_id: String,
    task_id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = store(&state)?;
    // Single atomic per-run transaction: validate run is Paused/Failed,
    // task is Blocked/Failed, retry_count < max_retries, then bump
    // retry_count, set Pending, and transition run to Running — all under
    // one lock so concurrent retry requests serialize.
    let next_retry = store
        .retry_blocked_task(&run_id, &task_id)
        .map_err(|e| match e {
            echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(msg) => {
                IpcError::Validation(msg)
            }
            other => internal(other),
        })?;
    tracing::info!(run_id = %run_id, task_id = %task_id, attempt = next_retry, "blocked task retried atomically -> run Running");

    // Resume the run via the standard execute_run path (mirrors resume_task_run).
    let primary_agent = state.app_state.connection.primary_agent();
    let store_for_task = store.clone();
    let primary_agent_for_task = primary_agent.clone();
    let run_store_for_task = primary_agent.read(|a| a.run_store().cloned()).await;
    let reviewer_llm = primary_agent.read(|a| a.llm_client().cloned()).await;
    let layer_manager = state
        .app_state
        .review_integration
        .as_ref()
        .map(|ri| std::sync::Arc::new(ri.create_layer_manager()));
    let cancel = echo_agent::agent::CancellationToken::new();
    let cancel_registration = match store.register_run_cancellation(&run_id, cancel.clone()) {
        Ok(registration) => registration,
        Err(error) => {
            let _ = store.transition_run(
                &run_id,
                echo_agent_app_core::tasks::task_runtime::TaskRunStatus::Paused,
            );
            return Err(internal(error));
        }
    };
    // Run was already transitioned to Running inside retry_blocked_task's
    // atomic section. Skip resume_task_run here — it would re-attempt the
    // Paused → Running transition and fail with IllegalTransition.
    let execution_projector = Arc::new(TauriExecutionProjector::new(
        app,
        state.app_state.storage.tool_executions.clone(),
        Some(store.clone()),
    ));
    let trace_sink: echo_agent_app_core::tasks::task_runtime::ExecSink = Arc::new(move |ev| {
        execution_projector.emit(ev);
    });
    let run_id_for_task = run_id.clone();
    // Wire the task lifecycle hook bridge (P0-5) for the retried GUI run.
    let retry_hook_bridge = primary_agent
        .read(|a| a.create_task_hook_bridge().bridge().clone())
        .await;
    let retry_subagent_bridge = primary_agent
        .read(|a| std::sync::Arc::new(a.create_subagent_hook_bridge()))
        .await;

    tokio::spawn(async move {
        let _cancel_registration = cancel_registration;
        let outcome = echo_agent_app_core::tasks::task_runtime::execute_run(
            store_for_task.clone(),
            Some(primary_agent_for_task),
            reviewer_llm,
            layer_manager,
            run_store_for_task,
            Some(trace_sink),
            &run_id_for_task,
            cancel,
            echo_agent_app_core::tasks::task_runtime::MemoryPolicy::FireAndForget,
            Some(retry_hook_bridge),
            Some(retry_subagent_bridge),
        )
        .await;
        match outcome {
            Ok(echo_agent_app_core::tasks::task_runtime::RunOutcome::Completed) => {
                tracing::info!(run_id = %run_id_for_task, "retried run completed");
            }
            Ok(other) => {
                tracing::warn!(run_id = %run_id_for_task, ?other, "retried run ended non-completed");
            }
            Err(e) => {
                tracing::error!(run_id = %run_id_for_task, error = %e, "retried run executor error");
            }
        }
    });

    Ok(serde_json::json!({
        "kind": "retry_scheduled",
        "run_id": run_id,
        "task_id": task_id,
        "next_attempt": next_retry,
    }))
}

/// Atomically update tasks and their relations against an expected revision.
#[tauri::command]
pub async fn update_tasks(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    request: TaskUpdateRequest,
) -> Result<TaskPlan, IpcError> {
    let store = store(&state)?;
    let agent = state.app_state.connection.primary_agent();
    let service = echo_agent_app_core::tasks::task_runtime::task_revision_service_for_agent(
        &agent,
        store.clone(),
    )
    .await;
    echo_agent_app_core::tasks::task_runtime::apply_eko_task_update(
        &service, &store, &run_id, request,
    )
    .await
    .map_err(|error| match error {
        echo_agent::tasks::TaskRevisionError::RevisionConflict { .. }
        | echo_agent::tasks::TaskRevisionError::InvalidInput { .. }
        | echo_agent::tasks::TaskRevisionError::TaskNotFound { .. }
        | echo_agent::tasks::TaskRevisionError::InvalidPatch { .. }
        | echo_agent::tasks::TaskRevisionError::PolicyRejected { .. }
        | echo_agent::tasks::TaskRevisionError::StoreRejected { .. } => {
            IpcError::Validation(error.to_string())
        }
        echo_agent::tasks::TaskRevisionError::GraphNotFound { .. }
        | echo_agent::tasks::TaskRevisionError::Backend { .. } => internal(error),
    })
}

/// Cancel an executing run. Cancels every in-flight subagent via the run's
/// CancellationToken and lets the executor wind down (the run ends Cancelled).
#[tauri::command]
pub async fn cancel_task_run(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = store(&state)?;
    let message_key = store
        .get_run(&run_id)
        .map_err(internal)?
        .map(|run| run.root_message_id);
    let cancelled = store.request_cancel(&run_id).map_err(internal)?;
    if cancelled {
        super::chat::cancel_pending_hitl(message_key.as_deref(), "task run cancelled").await;
    }
    Ok(serde_json::json!({
        "success": cancelled,
        "run_id": run_id,
    }))
}

/// Pause a running TaskRun through the same cancellation token that owns its
/// executor. Unlike cancellation, completed work remains resumable.
#[tauri::command]
pub async fn pause_task_run(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = store(&state)?;
    let message_key = store
        .get_run(&run_id)
        .map_err(internal)?
        .map(|run| run.root_message_id);
    let paused = store.request_pause(&run_id).map_err(internal)?;
    if paused {
        super::chat::cancel_pending_hitl(message_key.as_deref(), "task run paused").await;
    }
    Ok(serde_json::json!({
        "success": paused,
        "run_id": run_id,
    }))
}

/// Render the human-readable progress ledger derived from canonical run files.
/// It is also written to `.eko/runtime/{run_id}/progress.md` for compact agent
/// recovery context.
#[tauri::command]
pub async fn get_progress_ledger(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<String, IpcError> {
    let store = store(&state)?;
    // Use CWD as the workspace root — AppState::set_workspace does
    // std::env::set_current_dir to the workspace root, so CWD is correct
    // in the normal case. Falls back gracefully if CWD is unavailable.
    let base = std::env::current_dir().ok();
    echo_agent_app_core::tasks::task_runtime::write_progress(&store, &run_id, base.as_deref())
        .map_err(internal)
}
