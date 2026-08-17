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
use state::{TauriBridgeSupervisor, TauriState};
use std::sync::Arc;
use tauri::Emitter;

fn task_id_from_subagent_execution_id(execution_id: &str, run_id: &str) -> Option<String> {
    let scoped = execution_id.strip_prefix(run_id)?.strip_prefix(':')?;
    let mut parts = scoped.rsplitn(3, ':');
    let attempt = parts.next()?;
    let revision = parts.next()?;
    let task_id = parts.next()?;
    (!task_id.is_empty() && revision.parse::<u64>().is_ok() && attempt.parse::<u32>().is_ok())
        .then(|| task_id.to_string())
}

pub fn build_tauri_app(
    app_state: Arc<AppState>,
    browser_runtime: Arc<BrowserRuntime>,
    terminal_manager: Arc<terminal::TerminalManager>,
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
            terminal_manager,
            bridge_supervisor,
        ))
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
            // File-backed data analysis
            commands::analysis::list_analyses,
            commands::analysis::create_analysis,
            commands::analysis::get_analysis,
            commands::analysis::save_analysis,
            commands::analysis::run_analysis,
            commands::analysis::cancel_analysis,
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
            commands::workspace::exit_workspace,
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
            // TaskRuntime interaction mode (Chat/Task/Auto)
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
            commands::conversations::branch_conversation,
            commands::conversations::delete_conversation,
            commands::conversations::export_conversation,
            commands::conversations::restore_conversation,
            commands::conversations::search_conversations,
            // Chat streaming
            commands::chat::send_chat_message,
            commands::chat::steer_chat_message,
            commands::chat::get_active_chat_turn,
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
            commands::panels::check_skill_updates,
            commands::panels::sync_skills,
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
            commands::panels::get_mcp_server,
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

            #[cfg(debug_assertions)]
            if let Some(window) = app.get_webview_window("main") {
                window.open_devtools();
            }

            let browser_app_handle = app.handle().clone();
            let mut browser_events = app.state::<TauriState>().browser_runtime.subscribe();
            let browser_bridge_cancel = app
                .state::<TauriState>()
                .bridge_supervisor
                .cancellation_token();
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
            app.state::<TauriState>()
                .bridge_supervisor
                .track(browser_bridge);

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

            // Subagent execution event bridge — forward every SubagentEventBus
            // event onto the unified `execution://event` Tauri channel. The
            // pre-unification trace/event channels and the temp-id HashMap
            // were removed in Phase 4 of the Subagent
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
                let tool_executions = state.app_state.storage.tool_executions.clone();
                let supervisor = state.bridge_supervisor.clone();
                let cancel = supervisor.cancellation_token();
                let bridge = tokio::spawn(async move {
                    let subscription = agent.read_async(|a| {
                        Box::pin(async move { a.subagent_registry().event_bus().subscribe() })
                    });
                    let mut rx = tokio::select! {
                        _ = cancel.cancelled() => return,
                        rx = subscription => rx,
                    };
                    let mut usage_sequence_by_execution =
                        std::collections::HashMap::<String, u64>::new();
                    let mut subagent_context_by_execution =
                        std::collections::HashMap::<String, Option<String>>::new();
                    let mut active_tool_ids_by_execution = std::collections::HashMap::<
                        String,
                        std::collections::HashSet<String>,
                    >::new();
                    loop {
                        let event = tokio::select! {
                            _ = cancel.cancelled() => break,
                            event = rx.recv() => event,
                        };
                        match event {
                            Ok(event) => {
                                use echo_agent::agent::subagent::SubagentEvent;
                                if let SubagentEvent::DispatchStarted {
                                    execution_id: Some(execution_id),
                                    conversation_id,
                                    ..
                                } = event.as_ref()
                                {
                                    subagent_context_by_execution.insert(
                                        execution_id.clone(),
                                        conversation_id.clone(),
                                    );
                                }

                                match event.as_ref() {
                                    SubagentEvent::DispatchThinkingStarted { .. }
                                    | SubagentEvent::DispatchThinkingDelta { .. }
                                    | SubagentEvent::DispatchThinkingEnded { .. }
                                    | SubagentEvent::DispatchTokenDelta { .. } => continue,
                                    SubagentEvent::DispatchToolStarted {
                                        agent,
                                        call_id,
                                        invocation,
                                        execution_id,
                                        run_id,
                                        ..
                                    } => {
                                        let subagent_run_id = execution_id
                                            .clone()
                                            .unwrap_or_else(|| format!("{agent}:unknown"));
                                        let conversation_id = subagent_context_by_execution
                                            .get(&subagent_run_id)
                                            .cloned()
                                            .flatten()
                                            .or_else(|| {
                                                run_id.as_deref().and_then(|run_id| {
                                                    task_runtime_store
                                                        .as_ref()
                                                        .and_then(|store| store.get_run(run_id).ok())
                                                        .flatten()
                                                        .map(|run| run.conversation_id)
                                                })
                                            });
                                        let owner = echo_agent_app_core::tool_execution::ToolExecutionOwner::Subagent {
                                            subagent_run_id: subagent_run_id.clone(),
                                        };
                                        match tool_executions.start(
                                            owner,
                                            conversation_id.as_deref(),
                                            run_id.as_deref(),
                                            call_id,
                                            &invocation.name,
                                            &invocation.args,
                                        ) {
                                            Ok(summary) => {
                                                active_tool_ids_by_execution
                                                    .entry(subagent_run_id)
                                                    .or_default()
                                                    .insert(call_id.clone());
                                                commands::chat::emit_tool_execution_summary(
                                                    &app_handle,
                                                    "started",
                                                    agent,
                                                    &summary,
                                                );
                                            }
                                            Err(error) => {
                                                tracing::warn!(%error, %call_id, name = %invocation.name, "failed to persist subagent tool start");
                                            }
                                        }
                                        continue;
                                    }
                                    SubagentEvent::DispatchToolCompleted {
                                        agent,
                                        call_id,
                                        result,
                                        execution_id,
                                        ..
                                    } => {
                                        let subagent_run_id = execution_id
                                            .clone()
                                            .unwrap_or_else(|| format!("{agent}:unknown"));
                                        let owner = echo_agent_app_core::tool_execution::ToolExecutionOwner::Subagent {
                                            subagent_run_id: subagent_run_id.clone(),
                                        };
                                        match tool_executions.finish(
                                            &owner,
                                            call_id,
                                            result.success,
                                            result.error.as_deref().unwrap_or(&result.output),
                                            result.failure.clone(),
                                            result.metadata.clone(),
                                            result.truncated,
                                        ) {
                                            Ok(summary) => {
                                                if let Some(call_ids) = active_tool_ids_by_execution
                                                    .get_mut(&subagent_run_id)
                                                {
                                                    call_ids.remove(call_id);
                                                    if call_ids.is_empty() {
                                                        active_tool_ids_by_execution
                                                            .remove(&subagent_run_id);
                                                    }
                                                }
                                                commands::chat::emit_tool_execution_summary(
                                                    &app_handle,
                                                    "finished",
                                                    agent,
                                                    &summary,
                                                );
                                            }
                                            Err(error) => {
                                                tracing::warn!(%error, %call_id, "failed to persist subagent tool completion");
                                            }
                                        }
                                        continue;
                                    }
                                    SubagentEvent::DispatchCompleted {
                                        execution_id,
                                        agent,
                                        ..
                                    }
                                    | SubagentEvent::DispatchFailed {
                                        execution_id,
                                        agent,
                                        ..
                                    }
                                    | SubagentEvent::DispatchCancelled {
                                        execution_id,
                                        agent,
                                        ..
                                    } => {
                                        if let Some(subagent_run_id) = execution_id {
                                            let owner = echo_agent_app_core::tool_execution::ToolExecutionOwner::Subagent {
                                                subagent_run_id: subagent_run_id.clone(),
                                            };
                                            if let Some(call_ids) = active_tool_ids_by_execution
                                                .remove(subagent_run_id)
                                            {
                                                for call_id in call_ids {
                                                    match tool_executions.cancel(&owner, &call_id) {
                                                        Ok(summary) => {
                                                            commands::chat::emit_tool_execution_summary(
                                                                &app_handle,
                                                                "cancelled",
                                                                agent,
                                                                &summary,
                                                            );
                                                        }
                                                        Err(error) => {
                                                            tracing::warn!(%error, %call_id, "failed to cancel persisted subagent tool");
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                                let (event_type, execution_id, run_id, agent_name, extra) =
                                    match event.as_ref() {
                                        SubagentEvent::DispatchStarted {
                                            parent: _,
                                            agent,
                                            mode,
                                            task,
                                            execution_id,
                                            run_id,
                                            conversation_id,
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
                                                "conversation_id": conversation_id.clone(),
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
                                            result,
                                            execution_id,
                                            run_id,
                                        } => {
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
                                                    "terminal_status": result.status.as_str(),
                                                    "contract_version": result.contract_version,
                                                    "summary": result.summary.clone(),
                                                    "artifacts": result.artifacts.clone(),
                                                    "verification": result.verification.clone(),
                                                    "remaining_work": result.remaining_work.clone(),
                                                    "touched_files": result.touched_files.clone(),
                                                }),
                                            )
                                        },
                                        SubagentEvent::DispatchFailed {
                                            parent: _,
                                            agent,
                                            error,
                                            status,
                                            result,
                                            execution_id,
                                            run_id,
                                        } => (
                                            status.as_str(),
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({
                                                "error": error.clone(),
                                                "terminal_status": status.as_str(),
                                                "contract_version": result.contract_version,
                                                "summary": result.summary.clone(),
                                                "artifacts": result.artifacts.clone(),
                                                "verification": result.verification.clone(),
                                                "remaining_work": result.remaining_work.clone(),
                                                "touched_files": result.touched_files.clone(),
                                            }),
                                        ),
                                        SubagentEvent::DispatchCancelled {
                                            parent: _,
                                            agent,
                                            result,
                                            execution_id,
                                            run_id,
                                        } => (
                                            "cancelled",
                                            execution_id.clone(),
                                            run_id.clone(),
                                            agent.clone(),
                                            serde_json::json!({
                                                "terminal_status": result.status.as_str(),
                                                "contract_version": result.contract_version,
                                                "summary": result.summary.clone(),
                                                "artifacts": result.artifacts.clone(),
                                                "verification": result.verification.clone(),
                                                "remaining_work": result.remaining_work.clone(),
                                                "touched_files": result.touched_files.clone(),
                                            }),
                                        ),
                                        SubagentEvent::DispatchThinkingStarted { .. }
                                        | SubagentEvent::DispatchThinkingDelta { .. }
                                        | SubagentEvent::DispatchThinkingEnded { .. }
                                        | SubagentEvent::DispatchTokenDelta { .. }
                                        | SubagentEvent::DispatchToolStarted { .. }
                                        | SubagentEvent::DispatchToolCompleted { .. } => continue,
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
                                                "usage",
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
                                // `task_id` identifies the stable plan node, while
                                // `subagent_run_id` identifies this concrete attempt.
                                // Keeping `{run_id}:{task_id}:{plan_revision}:{attempt}` intact
                                // prevents late events from an older spec or retry
                                // overwriting newer lifecycle, usage, or terminal data.
                                let task_id_owned: Option<String> =
                                    execution_id.as_deref().and_then(|execution_id| {
                                        run_id.as_deref().and_then(|run_id| {
                                            task_id_from_subagent_execution_id(execution_id, run_id)
                                        })
                                    });
                                let subagent_run_id_owned: String = execution_id
                                    .clone()
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
                                if matches!(
                                    event_type,
                                    "completed" | "failed" | "cancelled" | "timed_out"
                                ) {
                                    subagent_context_by_execution.remove(&subagent_run_id_owned);
                                    usage_sequence_by_execution.remove(&subagent_run_id_owned);
                                    active_tool_ids_by_execution.remove(&subagent_run_id_owned);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("Subagent event receiver lagged by {} events", n);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
                supervisor.track(bridge);
            }

            Ok(())
        })
}

#[cfg(test)]
mod tests {
    use super::task_id_from_subagent_execution_id;

    #[test]
    fn execution_attempt_keeps_full_identity_and_extracts_only_the_task_join_key() {
        let execution_id = "run-1:phase:task-1:7:2";
        assert_eq!(
            task_id_from_subagent_execution_id(execution_id, "run-1").as_deref(),
            Some("phase:task-1")
        );
        assert_eq!(execution_id, "run-1:phase:task-1:7:2");
        assert!(task_id_from_subagent_execution_id("phase:task-1", "run-1").is_none());
        assert!(task_id_from_subagent_execution_id("run-2:phase:task-1:7:2", "run-1").is_none());
    }
}
