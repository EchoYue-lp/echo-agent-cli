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

use echo_agent_app_core::api::{AppState, browser::BrowserRuntime};
use state::{TauriBridgeSupervisor, TauriState};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tauri::Emitter;

pub fn build_tauri_app(
    app_state: Arc<AppState>,
    browser_runtime: Arc<BrowserRuntime>,
    subagent_projection: Arc<
        echo_agent_app_core::api::subagent_event_projection::SubagentEnvelopeProjectionService,
    >,
    bridge_supervisor: Arc<TauriBridgeSupervisor>,
) -> tauri::Builder<tauri::Wry> {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(TauriState::new(
            app_state,
            browser_runtime,
            subagent_projection,
            bridge_supervisor,
        ))
        .invoke_handler(tauri::generate_handler![
            // Native IPC (existing)
            ipc::native_read_file,
            ipc::native_write_file,
            ipc::native_notify,
            ipc::get_system_info,
            ipc::native_open_path,
            commands::extensions::execute_extension_command,
            commands::browser::chrome_setup_status,
            commands::browser::chrome_open_extensions_page,
            // File-backed data analysis
            commands::analysis::list_analyses,
            commands::analysis::create_analysis,
            commands::analysis::get_analysis,
            commands::analysis::save_analysis,
            commands::analysis::run_analysis,
            commands::analysis::cancel_analysis,
            commands::analysis::delete_analysis,
            // Cross-workspace Agent messaging
            commands::agent_router::list_agent_endpoints,
            commands::agent_router::get_agent_delivery_status,
            commands::agent_router::send_agent_message,
            commands::agent_router::list_agent_groups,
            commands::agent_router::create_agent_group,
            commands::agent_router::update_agent_group,
            commands::agent_router::delete_agent_group,
            // Config
            commands::config::get_config,
            commands::config::update_config,
            commands::config::get_full_config,
            commands::config::update_full_config,
            commands::config::discover_config,
            // Workspace
            commands::workspace::list_workspaces,
            commands::workspace::create_workspace,
            commands::workspace::create_and_switch_workspace,
            commands::workspace::get_default_root,
            commands::workspace::get_current_workspace,
            commands::workspace::get_workspace,
            commands::workspace::delete_workspace,
            commands::workspace::switch_workspace,
            commands::workspace::exit_workspace,
            commands::workspace::link_project,
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
            // TaskRuntime (complex-task runs, plans, todos, events, artifacts, reviews)
            commands::task_runtime::get_task_run,
            commands::task_runtime::get_task_completion_gate,
            commands::task_runtime::skip_task_goal_requirement,
            commands::task_runtime::get_task_continuation,
            commands::task_runtime::configure_task_continuation,
            commands::task_runtime::update_task_run_goal,
            commands::task_runtime::send_task_subagent_message,
            commands::task_runtime::queue_task_subagent_guidance,
            commands::task_runtime::interrupt_task_subagent,
            commands::task_runtime::list_task_background_cells,
            commands::task_runtime::latest_task_run_for_conversation,
            commands::task_runtime::list_task_runs,
            commands::task_runtime::get_task_plan,
            commands::task_runtime::list_task_todos,
            commands::task_runtime::list_task_events,
            commands::task_runtime::list_task_artifacts,
            commands::task_runtime::list_task_reviews,
            commands::task_runtime::get_task_summary,
            commands::task_runtime::list_recovery_blockers,
            commands::task_runtime::resolve_recovery_task,
            // TaskRuntime dynamic plan and recovery controls
            commands::task_runtime::resume_task_run,
            commands::task_runtime::retry_blocked_task,
            commands::task_runtime::update_tasks,
            commands::task_runtime::pause_task_run,
            commands::task_runtime::cancel_task_run,
            // TaskRuntime progress ledger (PR 4)
            commands::task_runtime::get_progress_ledger,
            // Memory
            commands::memory::list_memory,
            commands::memory::add_memory,
            commands::memory::search_memory,
            commands::memory::delete_memory,
            commands::memory::list_namespaces,
            commands::memory::reflect_session,
            // Tools
            commands::tools::list_tools,
            commands::tools::get_tool,
            commands::tools::enable_tool,
            commands::tools::disable_tool,
            commands::tool_executions::get_tool_execution_detail,
            commands::tool_executions::read_tool_execution_output,
            commands::tool_executions::list_tool_executions,
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
            commands::plugins::configure_plugin,
            commands::plugins::reload_plugins,
            commands::plugins::scaffold_plugin,
            commands::plugins::validate_plugin,
            commands::plugins::list_plugin_themes,
            commands::plugins::activate_plugin_theme,
            commands::plugins::list_plugin_output_styles,
            commands::plugins::activate_plugin_output_style,
            // Hooks
            commands::hooks::list_hooks,
            commands::hooks::list_hook_events,
            commands::hooks::reload_hooks,
            commands::hooks::test_hook,
            // LSP
            commands::lsp::lsp_control,
            // Providers
            commands::providers::list_model_providers,
            commands::providers::upsert_model_provider,
            commands::providers::delete_model_provider,
            commands::providers::list_configured_models,
            commands::providers::upsert_configured_model,
            commands::providers::delete_configured_model,
            commands::providers::set_default_model,
            commands::providers::test_connection,
            commands::providers::set_thinking,
            // Scheduler
            commands::scheduler::list_scheduler_tasks,
            commands::scheduler::add_scheduler_task,
            commands::scheduler::remove_scheduler_task,
            commands::scheduler::set_scheduler_task_status,
            commands::scheduler::run_scheduler_task,
            // Conversations
            commands::conversations::list_conversations,
            commands::conversations::set_conversation_archived,
            commands::conversations::save_conversation,
            commands::conversations::get_conversation,
            commands::conversations::update_conversation,
            commands::conversations::branch_conversation,
            commands::conversations::delete_conversation,
            commands::conversations::export_conversation,
            commands::conversations::restore_conversation,
            commands::conversations::search_conversations,
            // Chat streaming
            commands::chat::send_chat_message,
            commands::chat::steer_chat_message,
            commands::chat::get_active_chat_turn,
            commands::chat::replay_chat_events,
            commands::chat::queue_chat_input,
            commands::chat::list_queued_chat_inputs,
            commands::chat::remove_queued_chat_input,
            commands::chat::reorder_queued_chat_inputs,
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
            commands::panels::list_diagnostic_runs,
            commands::panels::get_run_diagnostics,
            commands::research::list_papers,
            commands::research::get_paper,
            commands::research::create_paper,
            commands::research::delete_paper,
            commands::research::update_paper_notes,
            commands::research::add_paper_tags,
            commands::research::list_research_evidence,
            commands::research::upsert_research_evidence,
            commands::research::delete_research_evidence,
            commands::research::list_systematic_reviews,
            commands::research::create_systematic_review,
            commands::research::get_systematic_review,
            commands::research::save_systematic_review,
            commands::research::delete_systematic_review,
            commands::research::search_scholarly_sources,
            commands::research::import_zotero_library,
            commands::research::export_zotero_library,
            commands::research::enrich_paper_europe_pmc,
            commands::research::audit_systematic_review,
            commands::research::export_systematic_review,
            commands::panels::review_run,
            commands::panels::list_evidence_candidates,
            commands::panels::evidence_candidate_action,
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
            commands::panels::cleanup_unattended_worktrees,
            // Terminal (PTY)
            terminal::create_terminal,
            terminal::write_terminal,
            terminal::resize_terminal,
            terminal::close_terminal,
            terminal::list_terminal_sessions,
        ])
        .setup(|app| {
            // Register global shortcut: CmdOrCtrl+Shift+E toggles window visibility
            use tauri::Manager;
            use tauri_plugin_global_shortcut::GlobalShortcutExt;

            // Keep normal `cargo gui-dev` usable. The inspector opens only for
            // an explicitly requested `devtools` build.
            #[cfg(all(debug_assertions, feature = "devtools"))]
            if let Some(window) = app.get_webview_window("main") {
                window.open_devtools();
            }

            let terminal_state = app.state::<TauriState>();
            let terminal_reservation = terminal_state.bridge_supervisor.reserve()?;
            let terminal_bridge = terminal::spawn_event_bridge(
                app.handle().clone(),
                terminal_state.app_state.terminal.clone(),
                terminal_state.bridge_supervisor.cancellation_token(),
            );
            terminal_reservation.track(terminal_bridge);

            let browser_app_handle = app.handle().clone();
            let mut browser_events = app.state::<TauriState>().browser_runtime.subscribe();
            let browser_bridge_cancel = app
                .state::<TauriState>()
                .bridge_supervisor
                .cancellation_token();
            let browser_reservation = app
                .state::<TauriState>()
                .bridge_supervisor
                .reserve()?;
            let browser_bridge = tokio::spawn(async move {
                loop {
                    let event = tokio::select! {
                        _ = browser_bridge_cancel.cancelled() => break,
                        event = browser_events.recv() => event,
                    };
                    match event {
                        Ok(event) => {
                            let _ = browser_app_handle.emit("browser://event", event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                            tracing::warn!(count, "browser event receiver lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            browser_reservation.track(browser_bridge);

            let subagent_state = app.state::<TauriState>();
            let subagent_projection = Arc::clone(&subagent_state.subagent_projection);
            let mut subagent_events = subagent_projection.subscribe_committed();
            let mut replayed_subagent_events =
                VecDeque::from(subagent_projection.replay_committed());
            let subagent_cancel = subagent_state.bridge_supervisor.cancellation_token();
            let subagent_reservation = subagent_state.bridge_supervisor.reserve()?;
            let subagent_app = app.handle().clone();
            let subagent_bridge = tokio::spawn(async move {
                let mut delivered_event_ids = HashSet::new();
                let mut delivered_event_order = VecDeque::new();
                loop {
                    let event = if let Some(event) = replayed_subagent_events.pop_front() {
                        event
                    } else {
                        let received = tokio::select! {
                            _ = subagent_cancel.cancelled() => break,
                            event = subagent_events.recv() => event,
                        };
                        match received {
                            Ok(event) => event,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                                tracing::warn!(count, "committed Subagent projection bridge lagged; replaying retained commits");
                                replayed_subagent_events
                                    .extend(subagent_projection.replay_committed());
                                continue;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    };
                    if !remember_committed_event(
                        &mut delivered_event_ids,
                        &mut delivered_event_order,
                        &event.envelope.event_id,
                    ) {
                        continue;
                    }
                    for update in &event.tool_updates {
                        let event_name = match update.kind {
                            echo_agent_app_core::api::tool_execution_projection::ToolExecutionProjectionKind::Started => "started",
                            echo_agent_app_core::api::tool_execution_projection::ToolExecutionProjectionKind::Finished => "finished",
                        };
                        commands::chat::emit_tool_execution_summary(
                            &subagent_app,
                            event_name,
                            &update.agent,
                            &update.summary,
                        );
                    }
                    let _ = commands::chat::emit_chat_envelope(&subagent_app, &event.envelope);
                }
            });
            subagent_reservation.track(subagent_bridge);

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

            Ok(())
        })
}

fn remember_committed_event(
    seen: &mut HashSet<String>,
    order: &mut VecDeque<String>,
    event_id: &str,
) -> bool {
    if !seen.insert(event_id.to_string()) {
        return false;
    }
    order.push_back(event_id.to_string());
    while order.len() > 2048 {
        if let Some(retired) = order.pop_front() {
            seen.remove(&retired);
        }
    }
    true
}
