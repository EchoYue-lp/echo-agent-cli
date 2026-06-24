//! Tauri IPC commands for the TaskRuntime.
//!
//! Read-only query commands, mutations for creating/managing task runs,
//! worker execution, and route feedback learning.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

use echo_agent_app_core::tasks::task_runtime::types::*;
use echo_agent_app_core::tasks::task_runtime::{
    ExecutionPolicy, ExecutionPolicySnapshot, RouteFeedbackRule, TaskRouteKind,
    save_route_feedback_rules,
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

/// Single GUI-facing snapshot of the execution policy. This intentionally
/// keeps Chat/Task/Auto mode, approval mode, and read-only worker fanout in
/// one place so the product can explain the runtime path before a message is
/// sent.
#[tauri::command]
pub async fn get_execution_policy(
    state: tauri::State<'_, TauriState>,
) -> Result<ExecutionPolicySnapshot, IpcError> {
    let interaction_mode = state
        .app_state
        .tasks
        .interaction_mode
        .load(std::sync::atomic::Ordering::Relaxed);
    let permission_mode = state.app_state.config.permission_mode.read().await.clone();
    Ok(ExecutionPolicy::from_raw(interaction_mode, permission_mode).snapshot())
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
    let previous = rules
        .iter()
        .find(|rule| route_feedback_key(&rule.pattern) == key)
        .cloned();
    rules.retain(|rule| route_feedback_key(&rule.pattern) != key);
    rules.push(RouteFeedbackRule {
        pattern,
        route,
        reason,
        suggested_workers: suggested_workers.unwrap_or_default(),
        hit_count: previous.as_ref().map(|rule| rule.hit_count).unwrap_or(0),
        last_matched_at: previous.and_then(|rule| rule.last_matched_at),
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

/// Request body for [`create_task_run`].
#[derive(Debug, serde::Deserialize)]
pub struct CreateRunRequest {
    pub goal: String,
    pub conversation_id: String,
    pub workspace_id: Option<String>,
    pub root_message_id: Option<String>,
    pub domain_profile: Option<String>,
}

/// Create a new complex-task run in `Pending`. Returns the created run.
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

    // spec §10.5: ComplexRuntime 审批闭环 — instead of directly transitioning to
    // Running + spawning execute_run (which would cause TWO concurrent execute_run
    // calls), we notify the approval_signal that the waiting execute_plan tool is
    // listening on. The awakened execute_plan handles Paused→Running + execute_run.
    if echo_agent_app_core::tasks::task_runtime::task_tools::notify_approval_signal(&run_id) {
        tracing::info!(run_id = %run_id, "notified approval_signal -> execute_plan will resume");
        return Ok(serde_json::json!({
            "kind": "resumed",
            "run_id": run_id,
        }));
    }

    // Fallback: no execute_plan tool is waiting. Do the direct path:
    // transition Paused -> Running + spawn executor.
    store.resume_task_run(&run_id).map_err(internal)?;
    tracing::info!(run_id = %run_id, "direct resume (no approval_signal) -> Running");

    let primary_agent = state.app_state.connection.primary_agent();
    let store_for_task = store.clone();
    let primary_agent_for_task = primary_agent.clone();
    let run_store_for_task = primary_agent.read(|a| a.run_store().cloned()).await;
    let (reviewer_llm, exec_cache_user_id) = primary_agent
        .read(|a| {
            (
                a.llm_client().cloned(),
                a.config().get_cache_user_id().map(|s| s.to_string()),
            )
        })
        .await;
    let exec_cache_user_id = exec_cache_user_id.unwrap_or_default();
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
    let run_id_for_task = run_id.clone();

    tokio::spawn(async move {
        let outcome = echo_agent_app_core::tasks::task_runtime::execute_run(
            store_for_task.clone(),
            Some(primary_agent_for_task),
            reviewer_llm,
            layer_manager,
            run_store_for_task,
            Some(trace_sink),
            &run_id_for_task,
            exec_cache_user_id.clone(),
            cancel,
        )
        .await;
        run_cancel_tokens.remove(&format!("__run__:{run_id_for_task}"));
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

/// Insert a new task into a run's plan. Works in any run state. The task is
/// inserted after `after_task_id` (or at the front if null). Validates
/// dependency integrity and acyclicity.
#[tauri::command]
pub async fn insert_task(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    after_task_id: Option<String>,
    task: PlanTask,
) -> Result<(), IpcError> {
    let store = store(&state)?;
    store
        .insert_task(&run_id, after_task_id, task)
        .map_err(internal)?;
    Ok(())
}

/// Soft-delete a task from a run's plan (marks it Skipped).
#[tauri::command]
pub async fn remove_task(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    task_id: String,
) -> Result<(), IpcError> {
    let store = store(&state)?;
    store.remove_task(&run_id, &task_id).map_err(internal)?;
    Ok(())
}

/// Update a task with a partial patch. Only non-None fields are applied.
/// Running tasks can only change title/description; terminal tasks reject
/// any update.
#[tauri::command]
pub async fn update_task(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    task_id: String,
    patch: echo_agent_app_core::tasks::task_runtime::types::TaskPatch,
) -> Result<(), IpcError> {
    let store = store(&state)?;
    store
        .update_task(&run_id, &task_id, patch)
        .map_err(internal)?;
    Ok(())
}

/// Reorder non-terminal tasks in a run's plan.
#[tauri::command]
pub async fn reorder_tasks(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    new_order: Vec<String>,
) -> Result<(), IpcError> {
    let store = store(&state)?;
    store.reorder_tasks(&run_id, new_order).map_err(internal)?;
    Ok(())
}

/// Launch execution of a run. The run must be in `Running`.
/// Execution runs on a detached background task so the IPC returns
/// immediately; progress is observable via `list_task_events` /
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
    if run.status != TaskRunStatus::Pending && run.status != TaskRunStatus::Running {
        return Err(IpcError::Validation(format!(
            "run {run_id} is {:?}; must be Pending or Running to execute",
            run.status
        )));
    }

    // Detached execution: the executor drives Running → terminal and
    // writes every transition + TaskEvent to the store. The GUI observes via
    // the read commands. A run-scoped CancellationToken is stored on the
    // session map (same mechanism as chat cancel) so cancel_task_run can find it.
    let store_for_task = store.clone();
    let primary_agent_for_task = primary_agent.clone();
    let run_store_for_task = primary_agent.read(|a| a.run_store().cloned()).await;
    let run_id_for_task = run_id.clone();
    // The reviewer LLM is the primary agent's client — review gates use it to
    // evaluate implementation/debugging task output against the domain checklist.
    let (reviewer_llm, exec_cache_user_id) = primary_agent
        .read(|a| {
            (
                a.llm_client().cloned(),
                a.config().get_cache_user_id().map(|s| s.to_string()),
            )
        })
        .await;
    let exec_cache_user_id = exec_cache_user_id.unwrap_or_default();
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
            exec_cache_user_id.clone(),
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

// ── Route feedback learning ───────────────────────────────────────────

#[tauri::command]
pub async fn submit_route_feedback(
    state: tauri::State<'_, TauriState>,
    message_hash: String,
    correction: String,
    note: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    use echo_agent_app_core::tasks::task_runtime::{
        RouteFeedbackAction, compute_scored_rules, load_route_records, route_feedback_matches,
    };

    let action = match correction.as_str() {
        "correct" => RouteFeedbackAction::Correct,
        "should_be_chat" => RouteFeedbackAction::ShouldBeChat,
        "should_be_task" => RouteFeedbackAction::ShouldBeTask,
        "should_be_readonly_parallel" => RouteFeedbackAction::ShouldBeReadonlyParallel,
        "too_many_workers" => RouteFeedbackAction::TooManyWorkers,
        "too_few_workers" => RouteFeedbackAction::TooFewWorkers,
        other => {
            return Err(IpcError::Internal(format!("unknown correction: {other}")));
        }
    };

    let mut records = load_route_records();
    let mut found = false;
    let mut learned_rule: Option<RouteFeedbackRule> = None;
    for record in &mut records {
        if record.message_hash == message_hash {
            record.user_correction = Some(action);
            if record.matched_feedback_pattern.is_none()
                && let Some(message_text) = record.message_text.as_deref()
            {
                let rules = state.app_state.tasks.route_feedback.read().await;
                record.matched_feedback_pattern = rules
                    .iter()
                    .find(|rule| route_feedback_matches(message_text, &rule.pattern))
                    .map(|rule| rule.pattern.clone());
            }
            if record.matched_feedback_pattern.is_none()
                && let Some(message_text) = record.message_text.as_deref()
                && let Some(target_route) = feedback_target_route(action, record.route)
            {
                let reason = note.clone().unwrap_or_else(|| {
                    format!(
                        "user corrected route from {} to {}",
                        record.route.as_str(),
                        target_route.as_str()
                    )
                });
                let rule = RouteFeedbackRule {
                    pattern: message_text.to_string(),
                    route: target_route,
                    reason,
                    suggested_workers: if target_route == TaskRouteKind::NormalChat {
                        Vec::new()
                    } else {
                        record
                            .actual_workers
                            .clone()
                            .unwrap_or_else(|| record.suggested_workers.clone())
                    },
                    hit_count: 0,
                    last_matched_at: None,
                };
                record.matched_feedback_pattern = Some(rule.pattern.clone());
                learned_rule = Some(rule);
            }
            found = true;
            break;
        }
    }

    // Also add a new record if no existing one matched
    if !found {
        let record = echo_agent_app_core::tasks::task_runtime::RouteDecisionRecord {
            message_hash: message_hash.clone(),
            message_text: None,
            route: echo_agent_app_core::tasks::task_runtime::TaskRouteKind::NormalChat,
            confidence: 0.0,
            matched_feedback_pattern: None,
            suggested_workers: vec![],
            actual_workers: None,
            final_run_status: None,
            user_correction: Some(action),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        records.push(record);
    }

    // Persist all records
    let path = echo_agent_app_core::tasks::task_runtime::default_route_records_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| IpcError::Internal(e.to_string()))?;
    }
    let content = records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, content).map_err(|e| IpcError::Internal(e.to_string()))?;

    if let Some(rule) = learned_rule {
        let key = route_feedback_key(&rule.pattern);
        let mut rules = state.app_state.tasks.route_feedback.write().await;
        let previous = rules
            .iter()
            .find(|existing| route_feedback_key(&existing.pattern) == key)
            .cloned();
        rules.retain(|existing| route_feedback_key(&existing.pattern) != key);
        rules.push(RouteFeedbackRule {
            hit_count: previous
                .as_ref()
                .map(|existing| existing.hit_count)
                .unwrap_or(0),
            last_matched_at: previous.and_then(|existing| existing.last_matched_at),
            ..rule
        });
        save_route_feedback_rules(&rules).map_err(internal)?;
    }

    // Recompute scored rules from feedback rules + records
    let rules = {
        let guard = state.app_state.tasks.route_feedback.read().await;
        guard.clone()
    };
    let scored = compute_scored_rules(&rules, &records);

    Ok(serde_json::json!({
        "success": true,
        "scored_rules": scored.iter().map(|s| serde_json::json!({
            "pattern": s.rule.pattern,
            "route": s.rule.route.as_str(),
            "hit_count": s.rule.hit_count,
            "success_count": s.success_count,
            "failure_count": s.failure_count,
            "score": s.score,
            "last_failure_reason": s.last_failure_reason,
        })).collect::<Vec<_>>(),
    }))
}

fn feedback_target_route(
    action: echo_agent_app_core::tasks::task_runtime::RouteFeedbackAction,
    current_route: TaskRouteKind,
) -> Option<TaskRouteKind> {
    use echo_agent_app_core::tasks::task_runtime::RouteFeedbackAction;
    match action {
        RouteFeedbackAction::Correct => None,
        RouteFeedbackAction::ShouldBeChat => Some(TaskRouteKind::NormalChat),
        RouteFeedbackAction::ShouldBeTask => Some(TaskRouteKind::ComplexRuntime),
        RouteFeedbackAction::ShouldBeReadonlyParallel => {
            Some(TaskRouteKind::ParallelReadonlyDelegation)
        }
        RouteFeedbackAction::TooManyWorkers | RouteFeedbackAction::TooFewWorkers => {
            Some(current_route)
        }
    }
}

#[tauri::command]
pub async fn get_scored_route_feedback_rules(
    state: tauri::State<'_, TauriState>,
) -> Result<Vec<serde_json::Value>, IpcError> {
    use echo_agent_app_core::tasks::task_runtime::{compute_scored_rules, load_route_records};

    let rules = {
        let guard = state.app_state.tasks.route_feedback.read().await;
        guard.clone()
    };
    let records = load_route_records();
    let scored = compute_scored_rules(&rules, &records);

    Ok(scored
        .iter()
        .map(|s| {
            serde_json::json!({
                "pattern": s.rule.pattern,
                "route": s.rule.route.as_str(),
                "hit_count": s.rule.hit_count,
                "success_count": s.success_count,
                "failure_count": s.failure_count,
                "score": s.score,
                "last_failure_reason": s.last_failure_reason,
            })
        })
        .collect())
}

// ── Conversation event replay ─────────────────────────────────────────

#[tauri::command]
pub async fn list_conversation_events(
    state: tauri::State<'_, TauriState>,
    conversation_id: String,
    since_seq: Option<String>,
) -> Result<Vec<serde_json::Value>, IpcError> {
    let store = store(&state)?;
    let since = since_seq.and_then(|s| s.parse::<i64>().ok());
    store
        .list_conversation_events(&conversation_id, since)
        .map_err(internal)
}

// ── Usage trend queries ───────────────────────────────────────────────

#[tauri::command]
pub async fn query_usage_records(
    state: tauri::State<'_, TauriState>,
    filter: echo_agent_app_core::tasks::task_runtime::UsageQueryFilter,
) -> Result<Vec<serde_json::Value>, IpcError> {
    let store = store(&state)?;
    let records = store.query_usage_records(&filter).map_err(internal)?;
    Ok(records
        .into_iter()
        .map(|r| serde_json::to_value(r).unwrap_or_default())
        .collect())
}

#[tauri::command]
pub async fn get_run_usage_summary(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<Option<serde_json::Value>, IpcError> {
    let store = store(&state)?;
    let summary = store.get_run_usage_summary(&run_id).map_err(internal)?;
    Ok(summary.map(|s| serde_json::to_value(s).unwrap_or_default()))
}
