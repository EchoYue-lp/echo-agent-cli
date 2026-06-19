//! Tauri desktop app module.
//!
//! The Tauri builder is configured here with native plugins and IPC commands.
//! All business logic goes through Tauri IPC — no embedded Axum server needed.

pub mod commands;
pub mod desktop;
pub mod error;
pub mod ipc;
pub mod state;
pub mod terminal;

use echo_agent_app_core::AppState;
use serde::Serialize;
use state::TauriState;
use std::sync::Arc;
use tauri::Emitter;

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

pub fn build_tauri_app(app_state: Arc<AppState>) -> tauri::Builder<tauri::Wry> {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(TauriState::new(app_state))
        .invoke_handler(tauri::generate_handler![
            // Native IPC (existing)
            ipc::native_read_file,
            ipc::native_write_file,
            ipc::native_notify,
            ipc::get_system_info,
            ipc::native_open_path,
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
            commands::files::diff_file,
            commands::files::file_tree,
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
            // TaskRuntime mutations (PR 2: planning runtime + plan approval)
            commands::task_runtime::create_task_run,
            commands::task_runtime::generate_task_plan,
            commands::task_runtime::approve_task_plan,
            commands::task_runtime::reject_task_plan,
            commands::task_runtime::edit_task_plan,
            // TaskRuntime execution (PR 3: DAG executor)
            commands::task_runtime::execute_task_run,
            commands::task_runtime::cancel_task_run,
            // TaskRuntime progress ledger (PR 4)
            commands::task_runtime::get_progress_ledger,
            // TaskRuntime router toggle
            commands::task_runtime::set_taskruntime_auto_route,
            commands::task_runtime::get_taskruntime_auto_route,
            // TaskRuntime approval scope
            commands::task_runtime::grant_approval_scope,
            // TaskRuntime interaction mode (Chat/Plan/Auto)
            commands::task_runtime::set_interaction_mode,
            commands::task_runtime::get_interaction_mode,
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
            commands::panels::list_papers,
            commands::panels::get_paper,
            commands::panels::create_paper,
            commands::panels::delete_paper,
            commands::panels::update_paper_notes,
            commands::panels::add_paper_tags,
            commands::panels::get_scratchpad,
            commands::panels::update_scratchpad,
            commands::panels::list_decisions,
            commands::panels::create_decision,
            commands::panels::clear_decisions,
            commands::panels::get_trajectories,
            commands::panels::get_trajectory_stats,
            commands::panels::review_trajectory,
            commands::panels::curator_action,
            commands::panels::list_worktrees,
            commands::panels::create_worktree,
            commands::panels::remove_worktree,
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
                .ok();

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

            Ok(())
        })
}
