//! Tauri IPC commands for the TaskRuntime.
//!
//! Read-only query commands, mutations for creating/managing task runs,
//! subagent execution, and route feedback learning.

use crate::tauri::commands::chat::TauriExecutionProjector;
use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

use echo_agent_app_core::state::ScopedChatRuntime;
use echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore;
use echo_agent_app_core::tasks::task_runtime::types::*;
use std::sync::Arc;

// ── Exact workspace runtime resolution ───────────────────────────────────

async fn task_runtime_for_workspace(
    state: &tauri::State<'_, TauriState>,
    workspace_id: &str,
) -> Result<(ScopedChatRuntime, Arc<TaskRuntimeStore>), IpcError> {
    if workspace_id.trim().is_empty() {
        return Err(IpcError::Validation(
            "workspace_id must not be empty".to_string(),
        ));
    }
    let runtime = state
        .app_state
        .chat_runtime_for_scope(workspace_id)
        .await
        .map_err(internal)?;
    let store = runtime.task_runtime().ok_or_else(|| {
        IpcError::Internal(format!(
            "TaskRuntime store is not initialized for workspace '{workspace_id}'"
        ))
    })?;
    validate_store_workspace(&store, workspace_id)?;
    Ok((runtime, store))
}

fn validate_store_workspace(store: &TaskRuntimeStore, workspace_id: &str) -> Result<(), IpcError> {
    let active_workspace_id = store.active_workspace_id();
    if active_workspace_id != workspace_id {
        return Err(IpcError::Internal(format!(
            "TaskRuntime scope mismatch: requested workspace '{workspace_id}', store owns '{active_workspace_id}'"
        )));
    }
    Ok(())
}

fn validate_run_workspace(run: &TaskRun, workspace_id: &str) -> Result<(), IpcError> {
    if run.workspace_id != workspace_id {
        return Err(IpcError::Validation(format!(
            "TaskRun '{}' belongs to workspace '{}', not requested workspace '{}'",
            run.run_id, run.workspace_id, workspace_id
        )));
    }
    Ok(())
}

fn get_scoped_run(
    store: &TaskRuntimeStore,
    workspace_id: &str,
    run_id: &str,
) -> Result<Option<TaskRun>, IpcError> {
    let run = store.get_run(run_id).map_err(internal)?;
    if let Some(run) = run.as_ref() {
        validate_run_workspace(run, workspace_id)?;
    }
    Ok(run)
}

fn require_scoped_run(
    store: &TaskRuntimeStore,
    workspace_id: &str,
    run_id: &str,
) -> Result<TaskRun, IpcError> {
    get_scoped_run(store, workspace_id, run_id)?.ok_or_else(|| {
        IpcError::NotFound(format!(
            "TaskRun '{run_id}' was not found in workspace '{workspace_id}'"
        ))
    })
}

async fn task_runtime_for_run(
    state: &tauri::State<'_, TauriState>,
    workspace_id: &str,
    run_id: &str,
) -> Result<(ScopedChatRuntime, Arc<TaskRuntimeStore>, TaskRun), IpcError> {
    let (runtime, store) = task_runtime_for_workspace(state, workspace_id).await?;
    let run = require_scoped_run(&store, workspace_id, run_id)?;
    Ok((runtime, store, run))
}

// ── Commands ─────────────────────────────────────────────────────────────

/// Fetch a single run by id.
#[tauri::command]
pub async fn get_task_run(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
) -> Result<Option<TaskRun>, IpcError> {
    let (_, store) = task_runtime_for_workspace(&state, &workspace_id).await?;
    get_scoped_run(&store, &workspace_id, &run_id)
}

/// The one Requirement/Evidence completion report shared by every surface.
#[tauri::command]
pub async fn get_task_completion_gate(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
) -> Result<CompletionGateReport, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store.completion_gate_report(&run_id).map_err(internal)
}

/// Confirm a Skip for one exact current-Goal requirement from the GUI.
#[tauri::command]
pub async fn skip_task_goal_requirement(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
    expected_goal_revision: u64,
    requirement_id: String,
    reason: String,
) -> Result<CompletionGateReport, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store
        .skip_goal_requirement(
            &run_id,
            expected_goal_revision,
            &requirement_id,
            &reason,
            RunGoalActorSource::Gui,
        )
        .map_err(internal)
}

/// Event-folded long-horizon control state for the existing TaskRun.
#[tauri::command]
pub async fn get_task_continuation(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
) -> Result<Option<RunContinuationState>, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store
        .get_run_state(&run_id)
        .map(|snapshot| snapshot.and_then(|state| state.continuation))
        .map_err(internal)
}

/// Update finite-turn budgets for an existing long-horizon TaskRun. Restart
/// auto-resume remains disabled until a surface can reconstruct its HITL owner;
/// changing a budget never bypasses an existing pause.
#[tauri::command]
pub async fn configure_task_continuation(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
    token_budget: Option<u64>,
    time_budget_seconds: Option<u64>,
) -> Result<RunContinuationState, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store
        .update_run_continuation_budgets(&run_id, token_budget, time_budget_seconds)
        .map_err(internal)
}

/// Update the authoritative Goal only from an explicit GUI action. The store
/// enforces paused/quiescent state and optimistic Goal revision matching.
#[tauri::command]
pub async fn update_task_run_goal(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
    expected_goal_revision: u64,
    new_goal: String,
    reason: String,
) -> Result<TaskRun, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store
        .update_run_goal(
            &run_id,
            expected_goal_revision,
            &new_goal,
            &reason,
            RunGoalActorSource::Gui,
        )
        .map_err(internal)
}

/// Send guidance through the safe point of one exact active Subagent attempt.
#[tauri::command]
pub async fn send_task_subagent_message(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    identity: SubagentControlIdentity,
    instruction: String,
) -> Result<SubagentControlReceipt, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &identity.run_id).await?;
    echo_agent_app_core::tasks::task_runtime::SubagentControlService::new(store)
        .send_message(identity, &instruction, SubagentControlActorSource::Gui)
        .await
        .map_err(internal)
}

/// Queue guidance for one exact future Subagent attempt.
#[tauri::command]
pub async fn queue_task_subagent_guidance(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    identity: SubagentControlIdentity,
    instruction: String,
) -> Result<SubagentControlReceipt, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &identity.run_id).await?;
    echo_agent_app_core::tasks::task_runtime::SubagentControlService::new(store)
        .queue_guidance(identity, &instruction, SubagentControlActorSource::Gui)
        .map_err(internal)
}

/// Interrupt one exact Subagent attempt without pausing its parent TaskRun.
#[tauri::command]
pub async fn interrupt_task_subagent(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    identity: SubagentControlIdentity,
) -> Result<SubagentControlReceipt, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &identity.run_id).await?;
    echo_agent_app_core::tasks::task_runtime::SubagentControlService::new(store)
        .interrupt_subagent(identity, SubagentControlActorSource::Gui)
        .await
        .map_err(internal)
}

/// Background command cells owned by the run, including bounded terminal facts.
#[tauri::command]
pub async fn list_task_background_cells(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
) -> Result<Vec<BackgroundCellState>, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store.list_background_cells(&run_id).map_err(internal)
}

/// Latest run for a conversation — binds a chat thread to its runtime run.
#[tauri::command]
pub async fn latest_task_run_for_conversation(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: String,
) -> Result<Option<TaskRun>, IpcError> {
    let (_, store) = task_runtime_for_workspace(&state, &workspace_id).await?;
    let run = store
        .latest_run_for_conversation(&conversation_id)
        .map_err(internal)?;
    if let Some(run) = run.as_ref() {
        validate_run_workspace(run, &workspace_id)?;
        if run.conversation_id != conversation_id {
            return Err(IpcError::Internal(format!(
                "TaskRuntime conversation mismatch: requested '{conversation_id}', resolved '{}'",
                run.conversation_id
            )));
        }
    }
    Ok(run)
}

/// All runs in any of the given statuses. Pass `None` or an empty list to
/// list every run (most recent first).
#[tauri::command]
pub async fn list_task_runs(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    statuses: Option<Vec<String>>,
) -> Result<Vec<TaskRun>, IpcError> {
    let (_, store) = task_runtime_for_workspace(&state, &workspace_id).await?;
    let parsed: Vec<TaskRunStatus> = statuses
        .unwrap_or_default()
        .iter()
        .filter_map(|s| TaskRunStatus::from_str(s))
        .collect();
    // Empty filter means every persisted status.
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
    let runs = store.list_runs_in(&query).map_err(internal)?;
    for run in &runs {
        validate_run_workspace(run, &workspace_id)?;
    }
    Ok(runs)
}

/// The structured plan attached to a run, or `None` if not yet generated.
#[tauri::command]
pub async fn get_task_plan(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
) -> Result<Option<TaskPlan>, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store.get_plan(&run_id).map_err(internal)
}

/// Todo projection for a run — what the right-rail Todo panel renders from.
#[tauri::command]
pub async fn list_task_todos(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
) -> Result<Vec<TodoItem>, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store.list_todos(&run_id).map_err(internal)
}

/// Events since `since_seq` (polling-style incremental event feed).
/// The GUI tracks the highest `seq` it has seen and polls with that value.
/// `since_seq` is a string because RuntimeTaskEvent.seq is serialized as a
/// string over IPC (i64 precision safety); we parse it back to i64 here.
#[tauri::command]
pub async fn list_task_events(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
    since_seq: Option<String>,
) -> Result<Vec<RuntimeTaskEvent>, IpcError> {
    let since = since_seq
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store.list_events(&run_id, since).map_err(internal)
}

/// Artifacts produced by a run (files, reports, charts, traces).
#[tauri::command]
pub async fn list_task_artifacts(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
) -> Result<Vec<Artifact>, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store.list_artifacts(&run_id).map_err(internal)
}

/// Reviews recorded against a task within a run (scoped to run_id + task_id
/// so task-id collisions across runs don't bleed history).
#[tauri::command]
pub async fn list_task_reviews(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
    task_id: String,
) -> Result<Vec<ReviewResult>, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store.list_reviews(&run_id, &task_id).map_err(internal)
}

/// The execution summary a subagent produced for a task — used by the Summary
/// Chain (downstream subagents consume this instead of raw chat history).
#[tauri::command]
pub async fn get_task_summary(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
    task_id: String,
) -> Result<Option<TaskExecutionSummary>, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store.get_summary(&run_id, &task_id).map_err(internal)
}

/// Recovery barriers created when a mutating subagent/tool was interrupted
/// between its durable start and terminal boundaries.
#[tauri::command]
pub async fn list_recovery_blockers(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
) -> Result<Vec<RecoveryBlocker>, IpcError> {
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store.list_recovery_blockers(&run_id).map_err(internal)
}

/// Resolve an indeterminate side effect after the user inspected the
/// workspace. Retry must use `retry_blocked_task` so driver admission,
/// generation pinning, and blocker classification remain one transaction.
#[tauri::command]
pub async fn resolve_recovery_task(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
    task_id: String,
    decision: String,
) -> Result<(), IpcError> {
    let decision = match decision.as_str() {
        "skip" => RecoveryDecision::Skip,
        _ => {
            return Err(IpcError::Validation(
                "recovery decision must be 'skip'; use retry_blocked_task for retry".to_string(),
            ));
        }
    };
    let (_, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    store
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
    workspace_id: String,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let (runtime, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    let run_state = store.get_run_state(&run_id).map_err(internal)?;
    if run_state
        .as_ref()
        .and_then(|snapshot| snapshot.continuation.as_ref())
        .is_some_and(|continuation| continuation.enabled)
    {
        return resume_continuation_run(&state, app, runtime, store, run_id, run_state).await;
    }
    let conversation_id = run_state
        .as_ref()
        .map(|snapshot| snapshot.run.conversation_id.clone())
        .ok_or_else(|| IpcError::Validation("TaskRun not found".to_string()))?;
    let pool_execution = runtime
        .agent_for(&conversation_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let primary_agent = pool_execution.agent();
    let review_integration = runtime.review_integration();
    let cancel = echo_agent::agent::CancellationToken::new();
    let supervisor_cancel = cancel.clone();
    let execution_projector = Arc::new(TauriExecutionProjector::new(
        app,
        state.app_state.storage.tool_executions.clone(),
        Some(store.clone()),
    ));
    let trace_sink: echo_agent_app_core::tasks::task_runtime::ExecSink = Arc::new(move |ev| {
        execution_projector.emit(ev);
    });
    let validation_store = store.clone();
    let validation_run_id = run_id.clone();
    let preparation_store = store.clone();
    let preparation_run_id = run_id.clone();
    store
        .spawn_supervised_run_driver(run_id.clone(), supervisor_cancel, move || {
            let memory_generation = review_integration
                .as_ref()
                .map(|integration| integration.lease_generation())
                .transpose()
                .map_err(|error| {
                    echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(format!(
                        "memory generation unavailable: {error}"
                    ))
                })?;
            let layer_manager = memory_generation
                .as_ref()
                .map(|generation| generation.create_layer_manager().map(Arc::new))
                .transpose()
                .map_err(|error| {
                    echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(format!(
                        "layered memory unavailable: {error}"
                    ))
                })?;
            if validation_store.get_plan(&validation_run_id)?.is_none() {
                return Err(
                    echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(format!(
                        "run {validation_run_id} has no persisted plan to resume"
                    )),
                );
            }
            Ok((memory_generation, layer_manager))
        }, move |(memory_generation, layer_manager)| {
            preparation_store.resume_task_run(&preparation_run_id)?;
            Ok(((), move |mut receipt_owner: echo_agent_app_core::tasks::task_runtime::RunDriverReceiptOwner| async move {
                let _pool_execution = pool_execution;
                if let Some(generation) = memory_generation.as_ref() {
                    receipt_owner.retain(generation.clone());
                }
                let run_store = primary_agent.read(|agent| agent.run_store().cloned()).await;
                let reviewer_llm = primary_agent
                    .read(|agent| agent.llm_client().cloned())
                    .await;
                let outcome = echo_agent_app_core::tasks::task_runtime::execute_run(
                    preparation_store.clone(),
                    Some(primary_agent),
                    reviewer_llm,
                    layer_manager,
                    memory_generation,
                    run_store,
                    Some(trace_sink),
                    &preparation_run_id,
                    cancel,
                    echo_agent_app_core::tasks::task_runtime::MemoryPolicy::BestEffortSettled,
                )
                .await;
                match outcome {
                    Ok(echo_agent_app_core::tasks::task_runtime::RunOutcome::Completed) => {
                        tracing::info!(run_id = %preparation_run_id, "resumed run completed");
                    }
                    Ok(other) => {
                        tracing::warn!(run_id = %preparation_run_id, ?other, "resumed run ended non-completed");
                    }
                    Err(error) => {
                        tracing::error!(run_id = %preparation_run_id, %error, "resumed run executor error");
                        return Err(error.to_string());
                    }
                }
                Ok(())
            }))
        })
        .map_err(internal)?;
    tracing::info!(run_id = %run_id, "task run resumed -> Running");

    Ok(serde_json::json!({
        "kind": "resumed",
        "run_id": run_id,
    }))
}

async fn resume_continuation_run(
    state: &tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    runtime: ScopedChatRuntime,
    store: Arc<TaskRuntimeStore>,
    run_id: String,
    run_state: Option<RunStateSnapshot>,
) -> Result<serde_json::Value, IpcError> {
    let snapshot =
        run_state.ok_or_else(|| IpcError::Validation("TaskRun not found".to_string()))?;
    if snapshot.run.status != TaskRunStatus::Paused {
        return Err(IpcError::Validation(format!(
            "long-horizon run {run_id} is {}; resume requires paused",
            snapshot.run.status.as_str()
        )));
    }
    if let Some(continuation) = snapshot.continuation.as_ref() {
        if continuation
            .token_budget
            .is_some_and(|budget| continuation.tokens_used >= budget)
        {
            return Err(IpcError::Validation(
                "the TaskRun token budget is exhausted; increase or remove the budget before resume"
                    .to_string(),
            ));
        }
        if continuation
            .time_budget_seconds
            .is_some_and(|budget| continuation.time_used_seconds >= budget)
        {
            return Err(IpcError::Validation(
                "the TaskRun time budget is exhausted; increase or remove the budget before resume"
                    .to_string(),
            ));
        }
    }
    let blockers = store.list_recovery_blockers(&run_id).map_err(internal)?;
    if !blockers.is_empty() {
        return Err(IpcError::Validation(format!(
            "run {run_id} has unresolved recovery blockers"
        )));
    }

    // Pause is durable before the cancelled finite turn necessarily releases
    // its exact driver. Do not mutate Paused -> Running until that old claim
    // has closed, otherwise an immediate resume can lose to its own old turn.
    store.wait_for_run_driver_idle(&run_id).await;
    let snapshot = store
        .get_run_state(&run_id)
        .map_err(internal)?
        .ok_or_else(|| {
            IpcError::Validation("TaskRun not found after driver settlement".to_string())
        })?;
    validate_run_workspace(&snapshot.run, runtime.execution_scope().workspace_id())?;
    if snapshot.run.status != TaskRunStatus::Paused {
        return Err(IpcError::Validation(format!(
            "long-horizon run {run_id} became {}; resume requires paused",
            snapshot.run.status.as_str()
        )));
    }
    if snapshot
        .continuation
        .as_ref()
        .is_some_and(|continuation| continuation.active_turn.is_some())
    {
        return Err(IpcError::Validation(format!(
            "long-horizon run {run_id} still has an active RunTurn after driver settlement"
        )));
    }

    let turn_id = uuid::Uuid::new_v4().to_string();
    let conversation_id = snapshot.run.conversation_id.clone();
    let root_message_id = snapshot.run.root_message_id.clone();
    let lease = runtime
        .begin_turn(
            &state.app_state.session.foreground_turns,
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Gui,
            &conversation_id,
            turn_id.clone(),
        )
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let pool_execution = match runtime.agent_for(&conversation_id).await {
        Ok(execution) => execution,
        Err(error) => {
            lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message("agent_pool", error.to_string()),
            ));
            return Err(IpcError::Validation(error.to_string()));
        }
    };
    let sink = crate::tauri::commands::chat::tauri_chat_sink(
        app.clone(),
        runtime.execution_scope().workspace_id().to_string(),
        turn_id.clone(),
        Some(conversation_id.clone()),
        state.app_state.storage.tool_executions.clone(),
        state.app_state.storage.chat_events.clone(),
    );
    let _ = sink.on_event(
        echo_agent_app_core::chat_driver::ChatDriverEvent::TurnStatus {
            status: "running".to_string(),
        },
    );
    let turn = echo_agent_app_core::prepared_turn::PreparedUserTurn::runtime_instruction(format!(
        "Resume the existing TaskRun {run_id} toward its unchanged Goal. Reload the authoritative runtime projection and continue the next useful work."
    ));
    let resources = Arc::new(echo_agent_app_core::chat_resources::ChatResources {
        execution_scope: runtime.execution_scope().clone(),
        pool: runtime.pool(),
        store: Some(store.clone()),
        sink: sink.clone(),
        webhook_emitter: Some(state.app_state.webhook.emitter.clone()),
        conv_id: Some(conversation_id.clone()),
        root_message_id: turn_id.clone(),
        attachments: snapshot.run.attachments,
        cancel: lease.cancellation_token(),
        interaction_mode: InteractionMode::Task,
        review_integration: runtime.review_integration(),
        layer_manager: None,
        memory_generation: None,
        human_loop_provider: Some(Arc::new(
            crate::tauri::commands::chat::TauriHumanLoopHandler::new(sink.clone(), turn_id.clone()),
        )),
    });
    let binding = RunTurnBinding {
        run_id: Some(run_id.clone()),
        turn_id: turn_id.clone(),
        root_message_id,
        origin: RunTurnOrigin::Resume,
        transcript_visibility: TurnVisibility::Internal,
    };
    let agent = pool_execution.agent();
    let spawned_run_id = run_id.clone();
    let status_store = store;
    tokio::spawn(async move {
        let _pool_execution = pool_execution;
        let outcome = echo_agent_app_core::foreground_turn::drive_foreground_chat_turn(
            lease, &agent, &turn, resources, binding,
        )
        .await;
        if let Err(error) = outcome.as_ref() {
            tracing::warn!(%error, run_id = %spawned_run_id, "long-horizon GUI resume failed");
        }
        let terminal_status = status_store
            .get_run(&spawned_run_id)
            .ok()
            .flatten()
            .map(|run| run.status.as_str().to_string())
            .unwrap_or_else(|| {
                outcome
                    .as_ref()
                    .map(|terminal| terminal.status().to_string())
                    .unwrap_or_else(|_| "failed".to_string())
            });
        let _ = sink.on_event(
            echo_agent_app_core::chat_driver::ChatDriverEvent::TurnStatus {
                status: terminal_status,
            },
        );
    });

    Ok(serde_json::json!({
        "kind": "continuation_resumed",
        "run_id": run_id,
        "turn_id": turn_id,
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
    workspace_id: String,
    run_id: String,
    task_id: String,
) -> Result<serde_json::Value, IpcError> {
    let (runtime, store, run) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    let pool_execution = runtime
        .agent_for(&run.conversation_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let primary_agent = pool_execution.agent();
    let review_integration = runtime.review_integration();
    let cancel = echo_agent::agent::CancellationToken::new();
    // The pinned retry facade selects recovery or acceptance and mutates the
    // run only after exact driver registration has completed.
    let execution_projector = Arc::new(TauriExecutionProjector::new(
        app,
        state.app_state.storage.tool_executions.clone(),
        Some(store.clone()),
    ));
    let trace_sink: echo_agent_app_core::tasks::task_runtime::ExecSink = Arc::new(move |ev| {
        execution_projector.emit(ev);
    });
    let preparation = spawn_tauri_task_retry(
        store,
        primary_agent,
        Some(pool_execution),
        review_integration,
        trace_sink,
        cancel,
        run_id.clone(),
        task_id.clone(),
    )
    .map_err(|error| match error {
        echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(message) => {
            IpcError::Validation(message)
        }
        other => internal(other),
    })?;
    let (kind, next_attempt) = match preparation {
        echo_agent_app_core::tasks::task_runtime::TaskRetryPreparation::Acceptance {
            next_attempt,
        } => ("retry_scheduled", Some(next_attempt)),
        echo_agent_app_core::tasks::task_runtime::TaskRetryPreparation::Recovery => {
            ("recovery_retry_recorded", None)
        }
    };
    tracing::info!(run_id = %run_id, task_id = %task_id, ?preparation, "task retry prepared atomically");
    Ok(serde_json::json!({
        "kind": kind,
        "run_id": run_id,
        "task_id": task_id,
        "next_attempt": next_attempt,
    }))
}

#[allow(clippy::too_many_arguments)]
fn spawn_tauri_task_retry(
    store: Arc<TaskRuntimeStore>,
    primary_agent: echo_agent_app_core::agent_handle::AgentHandle,
    pool_execution: Option<echo_agent_app_core::agent_pool::AgentPoolExecutionLease>,
    review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    trace_sink: echo_agent_app_core::tasks::task_runtime::ExecSink,
    cancel: echo_agent::agent::CancellationToken,
    run_id: String,
    task_id: String,
) -> Result<
    echo_agent_app_core::tasks::task_runtime::TaskRetryPreparation,
    echo_agent_app_core::tasks::task_runtime::StoreError,
> {
    let driver_store = store.clone();
    let driver_run_id = run_id.clone();
    let supervisor_cancel = cancel.clone();
    let (preparation, result_waiter) = store.spawn_supervised_task_retry(
        run_id,
        task_id,
        supervisor_cancel,
        move || {
            let memory_generation = review_integration
                .as_ref()
                .map(|integration| integration.lease_generation())
                .transpose()
                .map_err(|error| {
                    echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(format!(
                        "memory generation unavailable: {error}"
                    ))
                })?;
            let layer_manager = memory_generation
                .as_ref()
                .map(|generation| generation.create_layer_manager().map(Arc::new))
                .transpose()
                .map_err(|error| {
                    echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(format!(
                        "layered memory unavailable: {error}"
                    ))
                })?;
            Ok((memory_generation, layer_manager))
        },
        move |(memory_generation, layer_manager), mut receipt_owner| async move {
            let _pool_execution = pool_execution;
            if let Some(generation) = memory_generation.as_ref() {
                receipt_owner.retain(generation.clone());
            }
            let run_store = primary_agent.read(|agent| agent.run_store().cloned()).await;
            let reviewer_llm = primary_agent
                .read(|agent| agent.llm_client().cloned())
                .await;
            let outcome = echo_agent_app_core::tasks::task_runtime::execute_run(
                driver_store.clone(),
                Some(primary_agent),
                reviewer_llm,
                layer_manager,
                memory_generation,
                run_store,
                Some(trace_sink),
                &driver_run_id,
                cancel,
                echo_agent_app_core::tasks::task_runtime::MemoryPolicy::BestEffortSettled,
            )
            .await;
            match outcome {
                Ok(echo_agent_app_core::tasks::task_runtime::RunOutcome::Completed) => {
                    tracing::info!(run_id = %driver_run_id, "retried run completed");
                }
                Ok(other) => {
                    tracing::warn!(run_id = %driver_run_id, ?other, "retried run ended non-completed");
                }
                Err(error) => {
                    tracing::error!(run_id = %driver_run_id, %error, "retried run executor error");
                    return Err(error.to_string());
                }
            }
            Ok(())
        },
    )?;
    drop(result_waiter);
    Ok(preparation)
}

/// Atomically update tasks and their relations against an expected revision.
#[tauri::command]
pub async fn update_tasks(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    run_id: String,
    request: TaskUpdateRequest,
) -> Result<TaskPlan, IpcError> {
    let (runtime, store, run) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    let execution = runtime
        .agent_for(&run.conversation_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let agent = execution.agent();
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
    workspace_id: String,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let (_, store, run) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    let message_key = Some(run.root_message_id);
    let cancelled = store.request_cancel(&run_id).map_err(internal)?;
    if cancelled {
        super::chat::cancel_pending_hitl(message_key.as_deref(), "task run cancelled").await;
        store.wait_for_run_driver_idle(&run_id).await;
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
    workspace_id: String,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let (_, store, run) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    let message_key = Some(run.root_message_id);
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
    workspace_id: String,
    run_id: String,
) -> Result<String, IpcError> {
    let (runtime, store, _) = task_runtime_for_run(&state, &workspace_id, &run_id).await?;
    echo_agent_app_core::tasks::task_runtime::write_progress(
        &store,
        &run_id,
        Some(runtime.execution_scope().root()),
    )
    .map_err(internal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::error::Result as ReactResult;
    use echo_agent::llm::types::{ChatCompletionResponse, DeltaMessage, Message};
    use echo_agent::llm::{ChatChunk, ChatRequest, ChatResponse, LlmClient};
    use futures::future::BoxFuture;
    use futures::stream::{self, BoxStream};

    #[test]
    fn task_runtime_scope_validation_fails_closed() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        validate_store_workspace(&store, "test").map_err(|error| error.to_string())?;

        let store_error = validate_store_workspace(&store, "workspace-b")
            .err()
            .ok_or_else(|| "cross-workspace store validation unexpectedly succeeded".to_string())?;
        if !matches!(store_error, IpcError::Internal(_)) {
            return Err(format!(
                "cross-workspace store validation returned the wrong error: {store_error}"
            ));
        }

        store
            .create_run(
                "scope-run",
                "workspace-a",
                "scope-conversation",
                "scope-root",
                DomainProfile::General,
                "scope validation",
                "agent_task_plan",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let run = get_scoped_run(&store, "workspace-a", "scope-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "workspace A run is missing".to_string())?;
        assert_eq!(run.workspace_id, "workspace-a");

        let run_error = get_scoped_run(&store, "workspace-b", "scope-run")
            .err()
            .ok_or_else(|| "cross-workspace run validation unexpectedly succeeded".to_string())?;
        if !matches!(run_error, IpcError::Validation(_)) {
            return Err(format!(
                "cross-workspace run validation returned the wrong error: {run_error}"
            ));
        }
        Ok(())
    }

    struct TestLlmClient;

    impl LlmClient for TestLlmClient {
        fn chat(&self, _request: ChatRequest) -> BoxFuture<'_, ReactResult<ChatResponse>> {
            Box::pin(async {
                Ok(ChatResponse {
                    message: Message::assistant("done".to_string()),
                    finish_reason: Some("stop".to_string()),
                    usage: None,
                    raw: ChatCompletionResponse::default(),
                })
            })
        }

        fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> BoxFuture<'_, ReactResult<BoxStream<'static, ReactResult<ChatChunk>>>> {
            Box::pin(async {
                let chunks = vec![
                    Ok(ChatChunk {
                        delta: DeltaMessage {
                            content: Some("done".to_string()),
                            ..DeltaMessage::default()
                        },
                        finish_reason: None,
                        usage: None,
                    }),
                    Ok(ChatChunk {
                        delta: DeltaMessage::default(),
                        finish_reason: Some("stop".to_string()),
                        usage: None,
                    }),
                ];
                Ok(Box::pin(stream::iter(chunks)) as BoxStream<'static, ReactResult<ChatChunk>>)
            })
        }

        fn model_name(&self) -> &str {
            "test"
        }
    }

    fn test_agent() -> Result<echo_agent_app_core::agent_handle::AgentHandle, String> {
        echo_agent::agent::ReactAgentBuilder::new()
            .model("test")
            .llm_client(Arc::new(TestLlmClient))
            .build()
            .map(echo_agent_app_core::agent_handle::AgentHandle::new)
            .map_err(|error| error.to_string())
    }

    async fn prepare_retry_run(
        store: Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
        run_id: &str,
        task_id: &str,
        recovery: bool,
    ) -> Result<(), String> {
        store
            .create_run(
                run_id,
                "default",
                "tauri:test",
                "",
                DomainProfile::General,
                "retry from GUI",
                "agent_task_plan",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        echo_agent_app_core::tasks::task_runtime::commit_eko_task_plan(
            store.clone(),
            TaskPlan {
                plan_id: format!("{run_id}-plan"),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: echo_agent_app_core::tasks::task_runtime::task_goal_sha256(
                    "retry from GUI",
                ),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: task_id.to_string(),
                    title: "Retry task".to_string(),
                    max_retries: 2,
                    ..PlanTask::default()
                }],
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let (task_status, summary, run_status) = if recovery {
            (
                TodoStatus::Blocked,
                "mutating side effect is indeterminate after restart",
                TaskRunStatus::Paused,
            )
        } else {
            (
                TodoStatus::Failed,
                "acceptance failed",
                TaskRunStatus::Failed,
            )
        };
        store
            .set_task_status(run_id, task_id, task_status, None, Some(summary))
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, run_status)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    type RetryRuntimeSnapshot = (TaskRunStatus, Option<(TodoStatus, u32)>, usize);

    fn snapshot(
        store: &echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore,
        run_id: &str,
    ) -> Result<RetryRuntimeSnapshot, String> {
        let run = store
            .get_run(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("run missing: {run_id}"))?;
        let task = store
            .get_plan(run_id)
            .map_err(|error| error.to_string())?
            .and_then(|plan| {
                plan.tasks
                    .first()
                    .map(|task| (task.status, task.retry_count))
            });
        let event_count = store
            .list_events(run_id, 0)
            .map_err(|error| error.to_string())?
            .len();
        Ok((run.status, task, event_count))
    }

    fn trace_sink() -> echo_agent_app_core::tasks::task_runtime::ExecSink {
        Arc::new(|_| {})
    }

    #[tokio::test]
    async fn gui_retry_selects_acceptance_and_recovery_with_one_pinned_facade() -> Result<(), String>
    {
        for (run_id, recovery, expected) in [
            (
                "gui-acceptance",
                false,
                echo_agent_app_core::tasks::task_runtime::TaskRetryPreparation::Acceptance {
                    next_attempt: 1,
                },
            ),
            (
                "gui-recovery",
                true,
                echo_agent_app_core::tasks::task_runtime::TaskRetryPreparation::Recovery,
            ),
        ] {
            let store = Arc::new(
                echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                    .map_err(|error| error.to_string())?,
            );
            prepare_retry_run(store.clone(), run_id, "retry-task", recovery).await?;
            let preparation = spawn_tauri_task_retry(
                store.clone(),
                test_agent()?,
                None,
                None,
                trace_sink(),
                echo_agent::agent::CancellationToken::new(),
                run_id.to_string(),
                "retry-task".to_string(),
            )
            .map_err(|error| error.to_string())?;
            assert_eq!(preparation, expected);
            store
                .shutdown_run_drivers()
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn gui_recovery_retry_closed_admission_does_not_mutate_runtime() -> Result<(), String> {
        let store = Arc::new(
            echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        prepare_retry_run(store.clone(), "gui-closed", "retry-task", true).await?;
        let before = snapshot(&store, "gui-closed")?;
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        let result = spawn_tauri_task_retry(
            store.clone(),
            test_agent()?,
            None,
            None,
            trace_sink(),
            echo_agent::agent::CancellationToken::new(),
            "gui-closed".to_string(),
            "retry-task".to_string(),
        );
        assert!(result.is_err());
        assert_eq!(before, snapshot(&store, "gui-closed")?);
        Ok(())
    }

    #[test]
    fn gui_retry_registration_infrastructure_failure_does_not_mutate_runtime() -> Result<(), String>
    {
        let store = Arc::new(
            echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        runtime.block_on(prepare_retry_run(
            store.clone(),
            "gui-registration",
            "retry-task",
            false,
        ))?;
        drop(runtime);
        let before = snapshot(&store, "gui-registration")?;
        let error = spawn_tauri_task_retry(
            store.clone(),
            test_agent()?,
            None,
            None,
            trace_sink(),
            echo_agent::agent::CancellationToken::new(),
            "gui-registration".to_string(),
            "retry-task".to_string(),
        )
        .err()
        .ok_or_else(|| "GUI retry registration unexpectedly succeeded".to_string())?;
        assert!(
            error
                .to_string()
                .contains("requires an active Tokio runtime")
        );
        assert_eq!(before, snapshot(&store, "gui-registration")?);
        Ok(())
    }
}
