//! Tauri desktop app module.
//!
//! The Tauri builder is configured here with native plugins and IPC commands.
//! All business logic goes through Tauri IPC — no embedded Axum server needed.

pub mod commands;
pub mod desktop;
pub mod error;
pub mod ipc;
pub mod path_validator;
pub mod state;
pub mod terminal;

use echo_agent_app_core::{AppState, browser::BrowserRuntime};
use serde::Serialize;
use state::TauriState;
use std::sync::Arc;
use tauri::Emitter;

fn subagent_usage_event_name(
    event: &echo_agent::agent::subagent::SubagentEvent,
) -> Option<&'static str> {
    use echo_agent::agent::subagent::SubagentEvent;

    match event {
        SubagentEvent::DispatchThinkingEnded { .. } => Some("thinking_ended"),
        SubagentEvent::DispatchLlmUsage { .. } => Some("usage"),
        _ => None,
    }
}

/// Task event payload emitted to the frontend via `app.emit("task://event", ...)`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskEventPayload {
    /// Task created
    Created {
        task_id: String,
        description: String,
        kind: Option<String>,
    },
    /// Task status changed (includes cancellation as new_status: "cancelled")
    Updated {
        task_id: String,
        old_status: String,
        new_status: String,
    },
    /// Real-time progress update from ProgressBridge
    Progress {
        task_id: String,
        percentage: f64,
        phase: String,
        message: Option<String>,
        eta_secs: Option<u64>,
    },
    /// Task completed successfully
    Completed { task_id: String, result: String },
    /// Task failed
    Failed { task_id: String, error: String },
}

pub fn build_tauri_app(
    app_state: Arc<AppState>,
    browser_runtime: Arc<BrowserRuntime>,
) -> tauri::Builder<tauri::Wry> {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(TauriState::new(app_state, browser_runtime))
        .setup(|app| {
            use tauri::{Emitter, Manager};

            // Auto-open DevTools in debug builds so `cargo tauri dev` users can
            // inspect the WebView console immediately (Cmd+Option+I to toggle).
            #[cfg(debug_assertions)]
            {
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
            }

            let app_handle = app.handle().clone();
            let mut browser_events = app.state::<TauriState>().browser_runtime.subscribe();
            tokio::spawn(async move {
                loop {
                    match browser_events.recv().await {
                        Ok(event) => {
                            let _ = app_handle.emit("browser://event", event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                            tracing::warn!(count, "browser event receiver lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Native IPC (existing)
            ipc::native_read_file,
            ipc::native_write_file,
            ipc::native_notify,
            ipc::get_system_info,
            ipc::native_open_path,
            commands::browser::browser_navigate,
            commands::browser::browser_back,
            commands::browser::browser_reload,
            commands::browser::browser_screenshot,
            commands::browser::browser_click_at,
            commands::browser::browser_scroll,
            commands::browser::browser_tabs,
            commands::browser::browser_stop,
            commands::browser::browser_set_backend,
            commands::browser::chrome_setup_status,
            commands::browser::chrome_open_extensions_page,
            // Config
            commands::config::get_config,
            commands::config::update_config,
            commands::config::get_full_config,
            commands::config::update_full_config,
            commands::config::discover_config,
            // Workspace
            commands::workspace::list_workspaces,
            commands::workspace::create_workspace,
            commands::workspace::get_default_root,
            commands::workspace::get_current_workspace,
            commands::workspace::get_workspace,
            commands::workspace::delete_workspace,
            commands::workspace::switch_workspace,
            commands::workspace::link_project,
            commands::workspace::audit_migration,
            commands::workspace::execute_migration,
            // Session
            commands::session::get_session,
            commands::session::reset_session,
            commands::session::create_checkpoint,
            commands::session::list_checkpoints,
            commands::session::restore_checkpoint,
            commands::session::get_latest_session,
            // Files
            commands::files::list_files,
            commands::files::read_file,
            commands::files::write_file,
            commands::files::diff_file,
            commands::files::file_tree,
            commands::files::workspace_changes,
            commands::files::browse_directories,
            // Tasks
            commands::tasks::list_tasks,
            commands::tasks::submit_task,
            commands::tasks::get_task,
            commands::tasks::cancel_task,
            commands::tasks::get_task_dag,
            // TaskRuntime (complex-task runs, plans, todos, events, artifacts, reviews)
            commands::task_runtime::get_task_run,
            commands::task_runtime::latest_task_run_for_conversation,
            commands::task_runtime::list_task_runs,
            commands::task_runtime::get_task_plan,
            commands::task_runtime::list_task_todos,
            commands::task_runtime::list_task_events,
            commands::task_runtime::list_task_artifacts,
            commands::task_runtime::list_task_reviews,
            commands::task_runtime::get_task_summary,
            // TaskRuntime mutations (PR 2)
            commands::task_runtime::create_task_run,
            // TaskRuntime dynamic tasks (resume / insert / remove / update / reorder)
            commands::task_runtime::resume_task_run,
            commands::task_runtime::insert_task,
            commands::task_runtime::remove_task,
            commands::task_runtime::update_task,
            commands::task_runtime::reorder_tasks,
            // TaskRuntime execution (PR 3: DAG executor)
            commands::task_runtime::execute_task_run,
            commands::task_runtime::cancel_task_run,
            // TaskRuntime progress ledger (PR 4)
            commands::task_runtime::get_progress_ledger,
            // TaskRuntime interaction mode (Chat/Task/Auto)
            commands::task_runtime::set_interaction_mode,
            commands::task_runtime::get_interaction_mode,
            commands::task_runtime::get_execution_policy,
            commands::task_runtime::query_usage_records,
            commands::task_runtime::get_run_usage_summary,
            // Memory
            commands::memory::list_memory,
            commands::memory::add_memory,
            commands::memory::search_memory,
            commands::memory::delete_memory,
            commands::memory::list_namespaces,
            // Tools
            commands::tools::list_tools,
            commands::tools::get_tool,
            commands::tools::enable_tool,
            commands::tools::disable_tool,
            // MCP
            commands::mcp::list_mcp_servers,
            commands::mcp::connect_mcp_server,
            commands::mcp::disconnect_mcp_server,
            commands::mcp::toggle_mcp_server,
            commands::mcp::get_mcp_config,
            commands::mcp::update_mcp_config,
            // Plugins
            commands::plugins::list_plugins,
            commands::plugins::get_plugin,
            commands::plugins::install_plugin,
            commands::plugins::uninstall_plugin,
            commands::plugins::enable_plugin,
            commands::plugins::disable_plugin,
            commands::plugins::reload_plugins,
            // Providers
            commands::providers::list_model_templates,
            commands::providers::list_configured_models,
            commands::providers::upsert_configured_model,
            commands::providers::delete_configured_model,
            commands::providers::set_default_model,
            commands::providers::test_connection,
            commands::providers::get_thinking_support,
            commands::providers::set_thinking,
            // Scheduler
            commands::scheduler::list_scheduler_tasks,
            commands::scheduler::add_scheduler_task,
            commands::scheduler::remove_scheduler_task,
            commands::scheduler::set_scheduler_task_status,
            commands::scheduler::run_scheduler_task,
            // Conversations
            commands::conversations::list_conversations,
            commands::conversations::save_conversation,
            commands::conversations::get_conversation,
            commands::conversations::update_conversation,
            commands::conversations::delete_conversation,
            commands::conversations::export_conversation,
            commands::conversations::restore_conversation,
            commands::conversations::search_conversations,
            // Chat streaming
            commands::chat::send_chat_message,
            commands::chat::steer_chat_message,
            commands::chat::cancel_chat,
            commands::chat::send_approval_response,
            commands::chat::send_input_response,
            commands::chat::send_selection_response,
            // Panels (migrated from HTTP server)
            commands::panels::get_permissions_mode,
            commands::panels::set_permissions_mode,
            commands::panels::list_permission_rules,
            commands::panels::add_permission_rule,
            commands::panels::remove_permission_rule,
            commands::panels::get_audit_logs,
            commands::panels::get_audit_stats,
            commands::panels::clear_audit_logs,
            commands::panels::get_auto_memory_status,
            commands::panels::toggle_auto_memory,
            commands::panels::extract_auto_memory,
            commands::panels::get_auto_memory_observations,
            commands::panels::list_skills,
            commands::panels::get_skill,
            commands::panels::load_skill,
            commands::panels::enable_skill,
            commands::panels::disable_skill,
            commands::panels::upload_skill,
            commands::panels::list_workflows,
            commands::panels::get_workflow,
            commands::panels::create_workflow,
            commands::panels::delete_workflow,
            commands::panels::execute_workflow,
            commands::panels::get_sandbox_status,
            commands::panels::get_sandbox_config,
            commands::panels::update_sandbox_config,
            commands::panels::execute_sandbox,
            commands::panels::compress_context,
            commands::panels::get_compression_stats,
            commands::panels::extract_data,
            commands::panels::validate_schema,
            commands::panels::get_extract_examples,
            commands::panels::get_context_stats,
            commands::panels::get_history,
            commands::panels::export_history_markdown,
            commands::panels::export_history_json,
            commands::panels::list_trace_sessions,
            commands::panels::get_trace_events,
            commands::panels::get_trace_summary,
            commands::panels::clear_trace_session,
            commands::chat::get_cache_diagnostics,
            commands::panels::list_papers,
            commands::panels::get_paper,
            commands::panels::create_paper,
            commands::panels::delete_paper,
            commands::panels::update_paper_notes,
            commands::panels::add_paper_tags,
            commands::panels::get_trajectories,
            commands::panels::get_trajectory_stats,
            commands::panels::review_trajectory,
            commands::panels::curator_action,
            commands::panels::get_evolution_dashboard,
            commands::panels::scan_rule_proposals,
            commands::panels::promote_rule,
            commands::panels::scan_skill_candidates,
            commands::panels::generate_skill_draft,
            commands::panels::activate_skill_draft,
            commands::panels::list_worktrees,
            commands::panels::create_worktree,
            commands::panels::remove_worktree,
            commands::panels::list_unattended_worktrees,
            commands::panels::merge_unattended_worktree,
            commands::panels::discard_unattended_worktree,
            commands::panels::get_mcp_server,
            // Terminal (PTY)
            terminal::create_terminal,
            terminal::write_terminal,
            terminal::confirm_terminal_consent,
            terminal::resize_terminal,
            terminal::close_terminal,
            terminal::list_terminal_sessions,
        ])
        .setup(|app| {
            // Register global shortcut: CmdOrCtrl+Shift+E toggles window visibility
            use tauri::Manager;
            use tauri_plugin_global_shortcut::GlobalShortcutExt;

            let handle = app.handle().clone();
            app.global_shortcut()
                .on_shortcut("CmdOrCtrl+Shift+E", move |_app, _window, shortcut| {
                    if shortcut.state == tauri_plugin_global_shortcut::ShortcutState::Pressed
                        && let Some(window) = handle.get_webview_window("main")
                    {
                        if window.is_visible().unwrap_or(false) {
                            let _ = window.hide();
                        } else {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .unwrap_or_else(|e| {
                    // P2-1: 此前 .ok() 静默吞错, 快捷键被占用时用户无感知 (toggle 失效)。
                    tracing::warn!(error = %e, "Global shortcut CmdOrCtrl+Shift+E registration failed (likely held by another app)");
                });

            // Spawn task event emitter — bridges TaskEventBus to Tauri events
            let tauri_state = app.state::<TauriState>();
            if let Some(service) = tauri_state.app_state.tasks.service.as_ref() {
                let mut rx = service.subscribe_events();
                let app_handle = app.handle().clone();
                tokio::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(event) => {
                                let payload = match event.as_ref() {
                                    echo_agent_app_core::tasks::TaskEvent::Created { task } => {
                                        let kind = task
                                            .tags
                                            .iter()
                                            .find(|t| t.starts_with("bg:kind:"))
                                            .map(|t| {
                                                t.strip_prefix("bg:kind:").unwrap_or(t).to_string()
                                            });
                                        TaskEventPayload::Created {
                                            task_id: task.id.clone(),
                                            description: task.description.clone(),
                                            kind,
                                        }
                                    }
                                    echo_agent_app_core::tasks::TaskEvent::Updated {
                                        task_id,
                                        old_status,
                                        new_status,
                                    } => TaskEventPayload::Updated {
                                        task_id: task_id.clone(),
                                        old_status: format!("{:?}", old_status),
                                        new_status: format!("{:?}", new_status),
                                    },
                                    echo_agent_app_core::tasks::TaskEvent::Progress {
                                        task_id,
                                        progress,
                                    } => TaskEventPayload::Progress {
                                        task_id: task_id.clone(),
                                        percentage: progress.percentage,
                                        phase: progress.current_phase.clone(),
                                        message: progress.message.clone(),
                                        eta_secs: progress.eta_secs,
                                    },
                                    echo_agent_app_core::tasks::TaskEvent::Completed {
                                        task_id,
                                        result,
                                    } => TaskEventPayload::Completed {
                                        task_id: task_id.clone(),
                                        result: result.clone(),
                                    },
                                    echo_agent_app_core::tasks::TaskEvent::Failed {
                                        task_id,
                                        error,
                                        ..
                                    } => TaskEventPayload::Failed {
                                        task_id: task_id.clone(),
                                        error: error.clone(),
                                    },
                                    // Skip other events (Assigned, Deleted)
                                    _ => continue,
                                };
                                let _ = app_handle.emit("task://event", &payload);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("Task event receiver lagged by {} events", n);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                tracing::info!("Task event bus closed, stopping emitter");
                                break;
                            }
                        }
                    }
                });
            }

            // Subagent execution event bridge — forward every SubagentEventBus
            // event onto the unified `execution://event` Tauri channel. The
            // legacy `worker://trace` + `subagent://event` channels and the
            // temp-id HashMap were removed in Phase 4 of the Subagent
            // unification; the frontend now reads exclusively from
            // `execution://event` (keyed by the stable `execution_id` carried
            // on the event itself, no bridge-side allocation).
            //
            // Do NOT forward framework `parent` (parent agent *name*) as the
            // frontend `parent` field — that field means parent subagent_run_id
            // for nesting. Agent-name parent would fail ParallelExecutionBlock's
            // top-level filter and hide agent_tool cards.
            {
                let app_handle = app.handle().clone();
                let state = app.state::<TauriState>();
                let agent = state.app_state.connection.agent.clone();
                let task_runtime_store = state.app_state.tasks.runtime.clone();
                tokio::spawn(async move {
                    let mut rx = agent
                        .read_async(|a| {
                            Box::pin(async move { a.subagent_registry().event_bus().subscribe() })
                        })
                        .await;
                    let mut usage_sequence_by_execution =
                        std::collections::HashMap::<String, u64>::new();
                    loop {
                        match rx.recv().await {
                            Ok(event) => {
                                use echo_agent::agent::subagent::SubagentEvent;
                                let usage_event_type = subagent_usage_event_name(event.as_ref());
                                let (event_type, execution_id, run_id, agent_name, extra) =
                                    match event.as_ref() {
                                        SubagentEvent::DispatchStarted {
                                            parent: _,
                                            agent,
                                            mode,
                                            task,
                                            execution_id,
                                            run_id,
                                            message_id,
                                            background,
                                        } => (
                                            "started",
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({
                                                "mode": format!("{:?}", mode),
                                                "task": task.clone(),
                                                "message_id": message_id.clone(),
                                                "background": background,
                                            }),
                                        ),
                                        SubagentEvent::DispatchIsolationObserved {
                                            parent: _,
                                            agent,
                                            isolation,
                                            execution_id,
                                            run_id,
                                        } => (
                                            "isolation_observed",
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({
                                                "isolation_observed": isolation.as_str(),
                                            }),
                                        ),
                                        SubagentEvent::DispatchCompleted {
                                            parent: _,
                                            agent,
                                            duration_ms,
                                            tokens_used,
                                            iterations,
                                            output,
                                            execution_id,
                                            run_id,
                                        } => {
                                            let (summary, _) =
                                                echo_agent::agent::subagent::split_subagent_output(
                                                    output,
                                                );
                                            (
                                                "completed",
                                                execution_id.clone(),
                                                run_id.clone(),
                                                agent.clone(),
                                                serde_json::json!({
                                                    "duration_ms": duration_ms,
                                                    "tokens_used": tokens_used,
                                                    "iteration_count": iterations,
                                                    "output": output.clone(),
                                                    "summary": summary,
                                                }),
                                            )
                                        },
                                        SubagentEvent::DispatchFailed {
                                            parent: _,
                                            agent,
                                            error,
                                            execution_id,
                                            run_id,
                                        } => (
                                            "failed",
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({
                                                "error": error.clone(),
                                            }),
                                        ),
                                        SubagentEvent::DispatchCancelled {
                                            parent: _,
                                            agent,
                                            execution_id,
                                            run_id,
                                        } => (
                                            "cancelled",
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({}),
                                        ),
                                        SubagentEvent::DispatchThinkingStarted {
                                            parent: _,
                                            agent,
                                            execution_id,
                                            run_id,
                                        } => (
                                            "thinking_started",
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({}),
                                        ),
                                        SubagentEvent::DispatchThinkingDelta {
                                            parent: _,
                                            agent,
                                            content,
                                            execution_id,
                                            run_id,
                                        } => (
                                            "thinking_delta",
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({
                                                "content": content,
                                            }),
                                        ),
                                        SubagentEvent::DispatchThinkingEnded {
                                            parent: _,
                                            agent,
                                            prompt_tokens,
                                            completion_tokens,
                                            execution_id,
                                            run_id,
                                        } => (
                                            usage_event_type.unwrap_or("thinking_ended"),
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({
                                                "prompt_tokens": prompt_tokens,
                                                "completion_tokens": completion_tokens,
                                            }),
                                        ),
                                        SubagentEvent::DispatchTokenDelta {
                                            parent: _,
                                            agent,
                                            content,
                                            execution_id,
                                            run_id,
                                        } => (
                                            "token_delta",
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({
                                                "content": content,
                                            }),
                                        ),
                                        SubagentEvent::DispatchToolStarted {
                                            parent: _,
                                            agent,
                                            name: tool_name,
                                            args,
                                            execution_id,
                                            run_id,
                                        } => (
                                            "tool_started",
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({
                                                "name": tool_name,
                                                "args": args,
                                            }),
                                        ),
                                        SubagentEvent::DispatchToolCompleted {
                                            parent: _,
                                            agent,
                                            name: tool_name,
                                            result,
                                            success,
                                            execution_id,
                                            run_id,
                                        } => (
                                            "tool_completed",
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({
                                                "name": tool_name,
                                                "result": result,
                                                "success": success,
                                            }),
                                        ),
                                        SubagentEvent::DispatchLlmUsage {
                                            parent: _,
                                            agent,
                                            model,
                                            prompt_tokens,
                                            completion_tokens,
                                            total_tokens,
                                            cached_prompt_tokens,
                                            cache_creation_prompt_tokens,
                                            usage_reported,
                                            execution_id,
                                            run_id,
                                        } => {
                                            let usage_key = execution_id.clone().unwrap_or_else(|| {
                                                format!(
                                                    "{}:{}",
                                                    run_id.as_deref().unwrap_or("unknown-run"),
                                                    agent
                                                )
                                            });
                                            let sequence = usage_sequence_by_execution
                                                .entry(usage_key.clone())
                                                .or_insert(0);
                                            *sequence = sequence.saturating_add(1);
                                            let usage_event_id =
                                                format!("{usage_key}:usage:{sequence}");
                                            (
                                                usage_event_type.unwrap_or("usage"),
                                                execution_id.clone(),
                                                run_id.clone(),
                                                agent.clone(),
                                                serde_json::json!({
                                                    "model": model.clone(),
                                                    "prompt_tokens": prompt_tokens,
                                                    "completion_tokens": completion_tokens,
                                                    "total_tokens": total_tokens,
                                                    "cached_prompt_tokens": cached_prompt_tokens,
                                                    "cache_creation_prompt_tokens":
                                                        cache_creation_prompt_tokens,
                                                    "usage_reported": usage_reported,
                                                    "usage_event_id": usage_event_id,
                                                }),
                                            )
                                        }
                                        // Registered/Unregistered/Team* are not
                                        // execution-flow events; skip them.
                                        _ => continue,
                                    };
                                let mut payload = serde_json::Map::new();
                                payload.insert("kind".into(), "subagent".into());
                                // Extract the bare task_id from the framework's
                                // execution_id (format "{task_id}:{attempt}").
                                // We emit the bare task_id as BOTH `task_id` and
                                // `subagent_run_id` — the attempt suffix is dropped
                                // from subagent_run_id so retry attempts fold into
                                // one frontend card (matches Claude Code/Codex
                                // subagent display), and so the TaskRuntime bridge
                                // (chat.rs worker_trace_sink, which only has the
                                // bare task_id) emits the SAME key. P0.5 fix for the
                                // identity split bug (Q4): both bridges must agree
                                // on the key or the frontend splits one worker into
                                // two store records.
                                let task_id_owned: Option<String> = execution_id
                                    .as_deref()
                                    .and_then(|id| {
                                        id.split_once(':')
                                            .map(|(task_id, _)| task_id.to_string())
                                            .filter(|task_id| !task_id.is_empty())
                                    });
                                // subagent_run_id = bare task_id; fall back to the
                                // full execution_id only when it has no ':' (rare,
                                // e.g. a synthetic id without attempt).
                                let subagent_run_id_owned: String = task_id_owned
                                    .clone()
                                    .or_else(|| execution_id.clone())
                                    .unwrap_or_else(|| format!("{agent_name}:unknown"));
                                let task_id = task_id_owned.as_deref();
                                if let Some(task_id) = task_id {
                                    payload.insert("task_id".into(), task_id.into());
                                }
                                payload.insert(
                                    "subagent_run_id".into(),
                                    subagent_run_id_owned.clone().into(),
                                );
                                if let Some(run_id) = run_id.as_deref() {
                                    payload.insert("run_id".into(), run_id.into());
                                    if let Some(store) = task_runtime_store.as_ref()
                                        && let Ok(Some(run)) = store.get_run(run_id)
                                    {
                                        payload.insert(
                                            "conversation_id".into(),
                                            run.conversation_id.into(),
                                        );
                                        payload.insert(
                                            "message_id".into(),
                                            run.root_message_id.into(),
                                        );
                                    }
                                } else {
                                    payload.insert("run_id".into(), String::new().into());
                                }
                                payload.insert("agent".into(), agent_name.into());
                                payload.insert("event".into(), event_type.into());
                                if let serde_json::Value::Object(map) = extra {
                                    for (k, v) in map {
                                        payload.insert(k, v);
                                    }
                                }
                                let _ = app_handle
                                    .emit("execution://event", serde_json::Value::Object(payload));
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("Subagent event receiver lagged by {} events", n);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
            }

            Ok(())
        })
}

#[cfg(test)]
mod tests {
    use super::subagent_usage_event_name;
    use echo_agent::agent::subagent::SubagentEvent;

    #[test]
    fn thinking_ended_and_llm_usage_have_distinct_execution_event_names() {
        let thinking = SubagentEvent::DispatchThinkingEnded {
            parent: "main".to_string(),
            agent: "worker".to_string(),
            prompt_tokens: 10,
            completion_tokens: 2,
            execution_id: Some("task:1".to_string()),
            run_id: Some("run".to_string()),
        };
        let usage = SubagentEvent::DispatchLlmUsage {
            parent: "main".to_string(),
            agent: "worker".to_string(),
            model: "test".to_string(),
            prompt_tokens: 10,
            completion_tokens: 2,
            total_tokens: 12,
            cached_prompt_tokens: 0,
            cache_creation_prompt_tokens: 0,
            usage_reported: true,
            execution_id: Some("task:1".to_string()),
            run_id: Some("run".to_string()),
        };

        assert_eq!(subagent_usage_event_name(&thinking), Some("thinking_ended"));
        assert_eq!(subagent_usage_event_name(&usage), Some("usage"));
    }
}
