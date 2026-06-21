//! Tauri IPC commands for the TaskRuntime.
//!
//! PR 1 shipped read-only query commands. PR 2 adds the mutation commands
//! that drive the plan-approval lifecycle:
//!
//! - [`create_task_run`] — start a complex-task run in `Pending`.
//! - [`generate_task_plan`] — call the planner (LLM JSON mode) for a run and
//!   persist the structured plan; the run advances to `AwaitingPlanApproval`.
//! - [`approve_task_plan`] / [`reject_task_plan`] — user resolution of the
//!   plan; approve advances to `Ready`, reject to `Cancelled`.
//! - [`edit_task_plan`] — replace a plan's tasks before approving.
//!
//! The complex-task router itself (the `send_chat_message` classification
//! branch that auto-creates a run for complex input) is wired in the chat
//! command module, not here — these commands are the explicit,
//! user-controllable surface.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

use echo_agent_app_core::tasks::task_runtime::types::*;
use echo_agent_app_core::tasks::task_runtime::{
    RouteFeedbackRule, TaskRouteKind, save_route_feedback_rules,
};
use std::sync::Arc;
use tauri::Emitter;

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
            TaskRunStatus::Planning,
            TaskRunStatus::AwaitingPlanApproval,
            TaskRunStatus::Ready,
            TaskRunStatus::Running,
            TaskRunStatus::WaitingApproval,
            TaskRunStatus::WaitingInput,
            TaskRunStatus::Suspended,
            TaskRunStatus::Cancelling,
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

/// The execution summary a worker produced for a task — used by the Summary
/// Chain (downstream workers consume this instead of raw chat history).
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

// ── Error mapping ────────────────────────────────────────────────────────

fn internal<E: std::fmt::Display>(e: E) -> IpcError {
    IpcError::Internal(e.to_string())
}

// ── Router toggle ────────────────────────────────────────────────────────

/// Enable/disable auto-routing of complex chat input into the TaskRuntime.
/// Default OFF. When ON, `send_chat_message` classifies each message and, for
/// complex ones, creates a run + generates a plan instead of streaming chat.
/// Returns the new value.
#[tauri::command]
pub async fn set_taskruntime_auto_route(
    state: tauri::State<'_, TauriState>,
    enabled: bool,
) -> Result<bool, IpcError> {
    state
        .app_state
        .tasks
        .auto_route
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    tracing::info!(enabled, "taskruntime auto-route toggled");
    Ok(enabled)
}

/// Read the current auto-route flag.
#[tauri::command]
pub async fn get_taskruntime_auto_route(
    state: tauri::State<'_, TauriState>,
) -> Result<bool, IpcError> {
    Ok(state
        .app_state
        .tasks
        .auto_route
        .load(std::sync::atomic::Ordering::Relaxed))
}

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

/// List user-learned route feedback rules.
#[tauri::command]
pub async fn list_route_feedback_rules(
    state: tauri::State<'_, TauriState>,
) -> Result<Vec<RouteFeedbackRule>, IpcError> {
    Ok(state.app_state.tasks.route_feedback.read().await.clone())
}

/// Add or replace a route feedback rule and persist it to disk.
#[tauri::command]
pub async fn upsert_route_feedback_rule(
    state: tauri::State<'_, TauriState>,
    pattern: String,
    route: String,
    reason: String,
    suggested_workers: Option<Vec<String>>,
) -> Result<Vec<RouteFeedbackRule>, IpcError> {
    let pattern = pattern.trim().to_string();
    if pattern.is_empty() {
        return Err(IpcError::Validation(
            "route feedback pattern cannot be empty".to_string(),
        ));
    }
    let route = TaskRouteKind::from_str(route.trim())
        .ok_or_else(|| IpcError::Validation(format!("unknown route: {}", route.trim())))?;
    let key = route_feedback_key(&pattern);
    let mut rules = state.app_state.tasks.route_feedback.write().await;
    rules.retain(|rule| route_feedback_key(&rule.pattern) != key);
    rules.push(RouteFeedbackRule {
        pattern,
        route,
        reason,
        suggested_workers: suggested_workers.unwrap_or_default(),
    });
    save_route_feedback_rules(&rules).map_err(internal)?;
    Ok(rules.clone())
}

/// Delete a route feedback rule by pattern and persist the remaining rules.
#[tauri::command]
pub async fn delete_route_feedback_rule(
    state: tauri::State<'_, TauriState>,
    pattern: String,
) -> Result<Vec<RouteFeedbackRule>, IpcError> {
    let key = route_feedback_key(&pattern);
    let mut rules = state.app_state.tasks.route_feedback.write().await;
    rules.retain(|rule| route_feedback_key(&rule.pattern) != key);
    save_route_feedback_rules(&rules).map_err(internal)?;
    Ok(rules.clone())
}

fn route_feedback_key(pattern: &str) -> String {
    pattern
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ══════════════════════════════════════════════════════════════════════════
// Mutation commands (PR 2: planning runtime + plan-approval lifecycle)
// ══════════════════════════════════════════════════════════════════════════

/// Request body for [`create_task_run`].
#[derive(Debug, serde::Deserialize)]
pub struct CreateRunRequest {
    pub goal: String,
    pub conversation_id: String,
    pub workspace_id: Option<String>,
    pub root_message_id: Option<String>,
    pub domain_profile: Option<String>,
}

/// Create a new complex-task run in `Pending`. Does NOT generate a plan yet —
/// call [`generate_task_plan`] next. Returns the created run.
#[tauri::command]
pub async fn create_task_run(
    state: tauri::State<'_, TauriState>,
    req: CreateRunRequest,
) -> Result<TaskRun, IpcError> {
    let store = store(&state)?;
    let profile = req
        .domain_profile
        .as_deref()
        .and_then(DomainProfile::from_str)
        .unwrap_or_default();
    let run_id = uuid::Uuid::new_v4().to_string();
    let workspace_id = req.workspace_id.unwrap_or_else(|| "default".to_string());
    let root_message_id = req.root_message_id.unwrap_or_default();
    let run = store
        .create_run(
            &run_id,
            &workspace_id,
            &req.conversation_id,
            &root_message_id,
            profile,
            &req.goal,
        )
        .map_err(internal)?;
    tracing::info!(run_id = %run.run_id, profile = ?profile, "TaskRuntime run created");
    Ok(run)
}

/// Generate a structured plan for a run via the LLM (JSON mode), persist it,
/// and advance the run to `AwaitingPlanApproval`. The run must currently be
/// in `Pending` or `Planning`.
///
/// Uses the primary agent's LLM client. Returns the persisted plan plus any
/// non-blocking warnings the planner produced (e.g. a mutating task that
/// claimed a parallel group was serialized).
#[tauri::command]
pub async fn generate_task_plan(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<GeneratedPlanResponse, IpcError> {
    let store = store(&state)?;

    // Load the run to get goal + (inferred) profile.
    let run = store
        .get_run(&run_id)
        .map_err(internal)?
        .ok_or_else(|| IpcError::NotFound(format!("run {run_id} not found")))?;

    // Transition Pending -> Planning (allowed from Pending; idempotent-ish if
    // already Planning — that's a no-op illegal-transition we tolerate).
    if run.status == TaskRunStatus::Pending {
        store
            .transition_run(&run_id, TaskRunStatus::Planning)
            .map_err(internal)?;
    } else if run.status != TaskRunStatus::Planning {
        return Err(IpcError::Validation(format!(
            "run {run_id} is in state {:?}; must be Pending or Planning to generate a plan",
            run.status
        )));
    }

    // Obtain the LLM client from the primary agent.
    let llm = state
        .app_state
        .connection
        .primary_agent()
        .read(|a| a.llm_client().cloned())
        .await
        .ok_or_else(|| IpcError::Internal("no LLM client available on primary agent".into()))?;

    // Classify to steer the prompt with an inferred profile + reason. The
    // classifier is heuristic-only here; the run's existing profile wins if
    // the user already set one explicitly (we keep the run's profile).
    let classification =
        echo_agent_app_core::tasks::task_runtime::HeuristicClassifier::new().classify(&run.goal);
    let profile_for_plan = run.domain_profile;
    let classification = echo_agent_app_core::tasks::task_runtime::Classification {
        complexity: classification.complexity,
        inferred_profile: profile_for_plan,
        reason: classification.reason,
        signals: classification.signals,
    };

    let generated = echo_agent_app_core::tasks::task_runtime::generate_plan(
        &llm,
        &run_id,
        &run.goal,
        &classification,
        &[],
    )
    .await
    .map_err(|e| match e {
        echo_agent_app_core::tasks::task_runtime::PlanError::Quality(msg) => {
            IpcError::Validation(format!("plan rejected by quality check: {msg}"))
        }
        other => IpcError::Internal(format!("plan generation failed: {other}")),
    })?;

    // Persist + advance to AwaitingPlanApproval (attach_plan does both in one tx).
    store.attach_plan(&generated.plan).map_err(internal)?;
    tracing::info!(
        run_id = %run_id,
        plan_id = %generated.plan.plan_id,
        task_count = generated.plan.tasks.len(),
        warning_count = generated.warnings.len(),
        "TaskRuntime plan generated"
    );

    Ok(GeneratedPlanResponse {
        plan: generated.plan,
        warnings: generated.warnings,
    })
}

/// Response shape for [`generate_task_plan`].
#[derive(Debug, serde::Serialize)]
pub struct GeneratedPlanResponse {
    pub plan: TaskPlan,
    pub warnings: Vec<String>,
}

/// Approve the run's current plan and advance to `Ready` (ready for PR 3's
/// DAG executor to pick up). The run must be `AwaitingPlanApproval`.
#[tauri::command]
pub async fn approve_task_plan(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    note: Option<String>,
) -> Result<TaskRun, IpcError> {
    let store = store(&state)?;
    store
        .resolve_plan(&run_id, true, note.as_deref())
        .map_err(internal)?;
    let run = store
        .transition_run(&run_id, TaskRunStatus::Ready)
        .map_err(internal)?;
    tracing::info!(run_id = %run.run_id, "plan approved → Ready");
    Ok(run)
}

/// Reject the run's plan and cancel the run. The run must be
/// `AwaitingPlanApproval`.
#[tauri::command]
pub async fn reject_task_plan(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    note: Option<String>,
) -> Result<TaskRun, IpcError> {
    let store = store(&state)?;
    store
        .resolve_plan(&run_id, false, note.as_deref())
        .map_err(internal)?;
    // AwaitingPlanApproval -> Cancelled is a legal direct transition.
    let run = store
        .transition_run(&run_id, TaskRunStatus::Cancelled)
        .map_err(internal)?;
    tracing::info!(run_id = %run.run_id, "plan rejected → Cancelled");
    Ok(run)
}

/// Replace a run's plan tasks before approving. The run must be
/// `AwaitingPlanApproval`. Keeps the existing plan_id and goal; only the
/// task list changes (user-edited plan).
#[tauri::command]
pub async fn edit_task_plan(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    tasks: Vec<PlanTask>,
) -> Result<TaskPlan, IpcError> {
    let store = store(&state)?;
    let run = store
        .get_run(&run_id)
        .map_err(internal)?
        .ok_or_else(|| IpcError::NotFound(format!("run {run_id} not found")))?;
    if run.status != TaskRunStatus::AwaitingPlanApproval {
        return Err(IpcError::Validation(format!(
            "run {run_id} is {:?}; can only edit a plan while AwaitingPlanApproval",
            run.status
        )));
    }
    let mut plan = store
        .get_plan(&run_id)
        .map_err(internal)?
        .ok_or_else(|| IpcError::NotFound(format!("no plan for run {run_id}")))?;
    plan.tasks = tasks;
    // Re-persist via attach_plan (replaces tasks + todos atomically, stays in
    // AwaitingPlanApproval since the run is already there — attach_plan sets
    // it again idempotently).
    store.attach_plan(&plan).map_err(internal)?;
    tracing::info!(run_id = %run_id, task_count = plan.tasks.len(), "plan edited");
    Ok(plan)
}

/// Launch execution of an approved run. The run must be in `Ready` (i.e. the
/// plan was approved). Execution runs on a detached background task so the
/// IPC returns immediately; progress is observable via `list_task_events` /
/// `list_task_todos` polling or (PR 6) the GUI event feed. Returns immediately
/// with the run's id so the caller can track it.
#[tauri::command]
pub async fn execute_task_run(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = store(&state)?;
    // The primary agent is required: its subagent registry holds the worker
    // roles (project_explorer, code_reviewer, etc.) that the executor
    // dispatches to via delegate_to_agent_with_cancel. No pool is needed —
    // fork-mode dispatch runs workers on isolated instances under the
    // executor's own semaphore.
    let primary_agent = state.app_state.connection.primary_agent();

    // Snapshot the state for the validation error message.
    let run = store
        .get_run(&run_id)
        .map_err(internal)?
        .ok_or_else(|| IpcError::NotFound(format!("run {run_id} not found")))?;
    if run.status != TaskRunStatus::Ready {
        return Err(IpcError::Validation(format!(
            "run {run_id} is {:?}; must be Ready (approve the plan first)",
            run.status
        )));
    }

    // Idempotency: atomically transition Ready → Running BEFORE spawning.
    // This prevents a double-click / re-entrant call from spawning two
    // executors on the same run (TOCTOU between the check above and spawn).
    // If the transition fails (someone else already moved it), we get an
    // IllegalTransition error and return it as a validation error.
    store
        .transition_run(&run_id, TaskRunStatus::Running)
        .map_err(|e| match e {
            echo_agent_app_core::tasks::task_runtime::StoreError::IllegalTransition { .. } => {
                IpcError::Validation(format!(
                    "run {run_id} is no longer Ready (already executing?)"
                ))
            }
            other => internal(other),
        })?;

    // Detached execution: the executor drives Ready → Running → terminal and
    // writes every transition + TaskEvent to the store. The GUI observes via
    // the read commands. A run-scoped CancellationToken is stored on the
    // session map (same mechanism as chat cancel) so cancel_task_run can find it.
    let store_for_task = store.clone();
    let primary_agent_for_task = primary_agent.clone();
    let run_store_for_task = primary_agent.read(|a| a.run_store().cloned()).await;
    let run_id_for_task = run_id.clone();
    // The reviewer LLM is the primary agent's client — review gates use it to
    // evaluate implementation/debugging task output against the domain checklist.
    let reviewer_llm = primary_agent.read(|a| a.llm_client().cloned()).await;
    // The memory layer manager sinks run completion/cancellation events into
    // long-term memory through the single write_memory chokepoint. Created
    // from ReviewIntegration when available (mirrors the primary agent's path).
    let layer_manager = state
        .app_state
        .review_integration
        .as_ref()
        .map(|ri| std::sync::Arc::new(ri.create_layer_manager()));
    let cancel = echo_agent::agent::CancellationToken::new();
    let run_key = format!("__run__:{run_id}");
    state
        .app_state
        .tasks
        .run_cancel_tokens
        .insert(run_key.clone(), cancel.clone());
    let run_cancel_tokens = state.app_state.tasks.run_cancel_tokens.clone();
    let trace_sink: echo_agent_app_core::tasks::task_runtime::WorkerTraceSink =
        Arc::new(move |event| {
            let _ = app.emit("worker://trace", event);
        });

    tokio::spawn(async move {
        let outcome = echo_agent_app_core::tasks::task_runtime::execute_run(
            store_for_task.clone(),
            Some(primary_agent_for_task),
            reviewer_llm,
            layer_manager,
            run_store_for_task,
            Some(trace_sink),
            &run_id_for_task,
            cancel,
        )
        .await;
        run_cancel_tokens.remove(&format!("__run__:{run_id_for_task}"));
        match outcome {
            Ok(echo_agent_app_core::tasks::task_runtime::RunOutcome::Completed) => {
                tracing::info!(run_id = %run_id_for_task, "run completed");
            }
            Ok(other) => {
                tracing::warn!(run_id = %run_id_for_task, ?other, "run ended non-completed");
            }
            Err(e) => {
                tracing::error!(run_id = %run_id_for_task, error = %e, "run executor error");
            }
        }
    });

    Ok(serde_json::json!({
        "kind": "executing",
        "run_id": run_id,
    }))
}

/// Cancel an executing run. Cancels every in-flight worker via the run's
/// CancellationToken and lets the executor wind down (the run ends Cancelled).
#[tauri::command]
pub async fn cancel_task_run(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let key = format!("__run__:{run_id}");
    let cancelled = state
        .app_state
        .tasks
        .run_cancel_tokens
        .get(&key)
        .map(|t| {
            t.cancel();
            true
        })
        .unwrap_or(false);
    Ok(serde_json::json!({
        "success": cancelled,
        "run_id": run_id,
    }))
}

/// Render the human-readable progress ledger for a run (plan §866-901).
/// Derived from the canonical SQLite state; also written to
/// `.eko/runtime/{run_id}/progress.md` for agent recovery context. If the two
/// ever disagree, SQLite wins. Returns the rendered markdown.
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

/// Grant a scoped approval for tool calls within a run. Scope levels:
/// once | task | conversation | workspace | tool | all_tools.
/// The hitrisk re-check still applies regardless of scope.
#[tauri::command]
pub async fn grant_approval_scope(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    tool_name: String,
    scope_level: String,
    conversation_id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = store(&state)?;
    let created = store
        .grant_approval(&run_id, &tool_name, &scope_level, &conversation_id)
        .map_err(internal)?;
    Ok(serde_json::json!({ "granted": created }))
}
