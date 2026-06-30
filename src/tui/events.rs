//! TUI event loop — handles keyboard input, terminal resize, and agent streaming.

use super::{ChatMessage, MessageRole, TaskProgressEntry, TaskStripStatus, TuiApp};
use crate::agent_handle::AgentHandle;
use crate::tui::clipboard;
use crate::tui::commands::SlashCommand;
use crate::tui::ui;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

use echo_agent::tasks::TaskEvent;
use echo_agent_app_core::tasks::BackgroundTaskService;
use echo_agent_app_core::tasks::task_runtime::InteractionMode;

/// Poll interval for non-blocking event check.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Handle keyboard input when an approval request is pending.
/// Returns `true` if the key was consumed.
async fn handle_approval_key(
    _app: &mut TuiApp,
    pending_handle: &Arc<tokio::sync::Mutex<Option<echo_agent_app_core::hitl::PendingApproval>>>,
    key: &KeyEvent,
) -> bool {
    use echo_agent::human_loop::HumanLoopResponse;
    use echo_agent_app_core::hitl::PendingApproval;

    let mut guard = match pending_handle.try_lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let approval = match guard.as_mut() {
        Some(a) => a,
        None => return false,
    };

    if approval.input_mode {
        // ── Feedback input mode (for 拒绝/修改) ──
        match key.code {
            KeyCode::Esc => {
                // Cancel input, go back to option selection
                approval.input_mode = false;
                approval.feedback_input.clear();
                approval.feedback_cursor = 0;
                true
            }
            KeyCode::Enter => {
                // Submit feedback
                let label = approval.input_label.clone();
                let feedback = approval.feedback_input.clone();
                let reason = if feedback.is_empty() {
                    format!("用户{}", label)
                } else {
                    format!("用户{}: {}", label, feedback)
                };
                if let Some(tx) = approval.response_tx.take() {
                    let _ = tx.send(HumanLoopResponse::Rejected {
                        reason: Some(reason),
                    });
                }
                true
            }
            KeyCode::Backspace => {
                if approval.feedback_cursor > 0 {
                    // Remove character before cursor
                    let s = &mut approval.feedback_input;
                    let byte_idx = approval.feedback_cursor;
                    // Find the start of the previous character
                    let prev = s[..byte_idx]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    s.drain(prev..byte_idx);
                    approval.feedback_cursor = prev;
                }
                true
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return false; // Let global shortcuts through
                }
                approval.feedback_input.insert(approval.feedback_cursor, c);
                approval.feedback_cursor += c.len_utf8();
                true
            }
            KeyCode::Left => {
                if approval.feedback_cursor > 0 {
                    let s = &approval.feedback_input;
                    approval.feedback_cursor = s[..approval.feedback_cursor]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
                true
            }
            KeyCode::Right => {
                let s = &approval.feedback_input;
                if approval.feedback_cursor < s.len() {
                    approval.feedback_cursor += s[approval.feedback_cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(0);
                }
                true
            }
            _ => true, // Consume all other keys in input mode
        }
    } else {
        // ── Option selection mode ──
        match key.code {
            KeyCode::Left => {
                if approval.selected_option > 0 {
                    approval.selected_option -= 1;
                }
                true
            }
            KeyCode::Right | KeyCode::Tab => {
                approval.selected_option =
                    (approval.selected_option + 1) % PendingApproval::OPTION_COUNT;
                true
            }
            KeyCode::Enter => {
                // Confirm selected option
                send_approval_response(approval);
                true
            }
            KeyCode::Char('y') => {
                approval.selected_option = 0;
                send_approval_response(approval);
                true
            }
            KeyCode::Char('n') => {
                approval.selected_option = 1;
                approval.input_mode = true;
                approval.input_label = "拒绝原因".to_string();
                approval.feedback_input.clear();
                approval.feedback_cursor = 0;
                true
            }
            KeyCode::Char('m') => {
                approval.selected_option = 2;
                approval.input_mode = true;
                approval.input_label = "修改意见".to_string();
                approval.feedback_input.clear();
                approval.feedback_cursor = 0;
                true
            }
            KeyCode::Char('a') => {
                approval.selected_option = 3;
                send_approval_response(approval);
                true
            }
            KeyCode::Esc => {
                // Esc = reject
                if let Some(tx) = approval.response_tx.take() {
                    let _ = tx.send(HumanLoopResponse::Rejected {
                        reason: Some("User dismissed".to_string()),
                    });
                }
                true
            }
            _ => false, // Let other keys through
        }
    }
}

/// Send the approval response based on the currently selected option.
fn send_approval_response(approval: &mut echo_agent_app_core::hitl::PendingApproval) {
    use echo_agent::human_loop::{ApprovalScope, HumanLoopResponse};

    let response = match approval.selected_option {
        0 => HumanLoopResponse::Approved,
        1 => {
            // Switch to input mode for rejection reason
            approval.input_mode = true;
            approval.input_label = "拒绝原因".to_string();
            approval.feedback_input.clear();
            approval.feedback_cursor = 0;
            return; // Don't send yet
        }
        2 => {
            // Switch to input mode for modification feedback
            approval.input_mode = true;
            approval.input_label = "修改意见".to_string();
            approval.feedback_input.clear();
            approval.feedback_cursor = 0;
            return; // Don't send yet
        }
        3 => HumanLoopResponse::ApprovedWithScope {
            scope: ApprovalScope::SessionAllTools,
        },
        _ => HumanLoopResponse::Approved,
    };

    if let Some(tx) = approval.response_tx.take() {
        let _ = tx.send(response);
    }
}

enum AgentEvent {
    /// A streaming token chunk from the LLM.
    Token(String),
    /// LLM thinking/reasoning started.
    ThinkStart,
    /// LLM thinking/reasoning ended.
    ThinkEnd {
        prompt_tokens: usize,
        completion_tokens: usize,
    },
    /// A tool batch is starting (tools between start and end are concurrent).
    ToolBatchStart { tool_count: usize },
    /// A tool batch has ended.
    ToolBatchEnd,
    /// The final complete answer from the agent.
    FinalAnswer(String),
    /// A tool is about to be called.
    ToolCall { name: String, args: String },
    /// A tool execution completed.
    ToolResult { name: String, output: String },
    /// An error occurred.
    Error(String),
    /// Context was auto-compressed to fit within token limits.
    ContextCompressed {
        before_count: usize,
        after_count: usize,
        before_tokens: usize,
        after_tokens: usize,
    },
}

/// Run the main event loop.
pub async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
    agent: AgentHandle,
    task_service: Option<Arc<BackgroundTaskService>>,
) -> anyhow::Result<()> {
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();

    // Subscribe to task events for the parallel task progress strip.
    let mut task_event_rx: Option<broadcast::Receiver<Arc<TaskEvent>>> =
        task_service.as_ref().map(|svc| svc.subscribe_events());

    loop {
        // ── Drain task events into parallel_tasks ──────────────────────
        if let Some(ref mut rx) = task_event_rx {
            while let Ok(event) = rx.try_recv() {
                update_parallel_tasks(app, &event);
            }
        }

        // Pre-compute chat area and wrapped lines for mouse selection.
        let size = terminal.size()?;
        let screen = Rect::new(0, 0, size.width, size.height);
        app.chat_area = TuiApp::compute_chat_rect(screen, app.sidebar_visible);
        app.update_wrapped_lines(app.chat_area.width);

        // Flush buffered streaming tokens (throttled to ~2 updates/sec).
        app.flush_pending_stream();

        // Rebuild chat line cache if stale (avoids expensive markdown re-render).
        app.prepare_chat_cache();

        // Draw UI.
        terminal.draw(|f| ui::draw(f, app))?;

        while let Ok(event) = agent_rx.try_recv() {
            match event {
                AgentEvent::Token(chunk) => {
                    app.append_stream(&chunk);
                }
                AgentEvent::ThinkStart => {
                    // Start a new thinking phase — insert a visible marker
                    app.iteration_count += 1;
                }
                AgentEvent::ThinkEnd {
                    prompt_tokens,
                    completion_tokens,
                } => {
                    // Accumulate token usage for stats display
                    app.tokens.0 = app.tokens.0.saturating_add(prompt_tokens as u32);
                    app.tokens.1 = app.tokens.1.saturating_add(completion_tokens as u32);
                    app.tokens.2 += 1; // request count
                }
                AgentEvent::ToolBatchStart { tool_count } => {
                    tracing::debug!(tool_count, "TUI tool batch started");
                    // Round boundary — current thinking phase is done, tools follow
                }
                AgentEvent::ToolBatchEnd => {
                    // Tool batch complete — rebuild groups to reflect round structure
                }
                AgentEvent::FinalAnswer(_answer) => {
                    app.finalize_stream();
                }
                AgentEvent::ToolCall { name, args } => {
                    let display = if args.chars().count() > 2000 {
                        let truncated: String = args.chars().take(2000).collect();
                        format!("{} ({}...)", name, truncated)
                    } else {
                        format!("{} ({})", name, args)
                    };
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("🔧 调用工具: {}", display),
                    });
                }
                AgentEvent::ToolResult { name, output } => {
                    // File-editing tools get a dedicated diff display
                    if matches!(name.as_str(), "edit_file" | "create_file" | "write_file") {
                        app.messages.push(ChatMessage {
                            role: MessageRole::ToolResult {
                                tool_name: name.clone(),
                            },
                            content: output,
                        });
                    } else {
                        let display = if output.chars().count() > 100 {
                            let truncated: String = output.chars().take(100).collect();
                            format!("{} → {}...", name, truncated)
                        } else {
                            format!("{} → {}", name, output)
                        };
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!("✓ {}", display),
                        });
                    }
                    app.rebuild_message_groups();
                }
                AgentEvent::Error(e) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Error: {e}"),
                    });
                    app.is_processing = false;
                    app.status_msg = "Error".to_string();
                }
                AgentEvent::ContextCompressed {
                    before_count,
                    after_count,
                    before_tokens,
                    after_tokens,
                } => {
                    let saved = before_tokens.saturating_sub(after_tokens);
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!(
                            "🗜️ 上下文压缩: {}→{} 条 ({}→{} tokens, 节省 {})",
                            before_count, after_count, before_tokens, after_tokens, saved
                        ),
                    });
                    app.rebuild_message_groups();
                }
            }
        }

        // Handle events.
        // ── Resilient event reading: tolerate transient I/O errors ──
        // On macOS, terminal resize generates SIGWINCH which can interrupt
        // crossterm's underlying read() syscall with EINTR. We must NOT
        // propagate these as fatal errors — just skip the tick and retry.
        match event::poll(POLL_INTERVAL) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => handle_key(app, key, &agent, agent_tx.clone()).await,
                Ok(Event::Mouse(mouse)) => handle_mouse(app, &mouse),
                Ok(Event::Resize(_, _)) => {} // ratatui handles resize automatically
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    tracing::warn!("crossterm event::read() error: {e}");
                    // Non-fatal: skip this tick, will retry on next loop iteration.
                    // Only propagate truly unexpected errors.
                    if e.kind() != io::ErrorKind::WouldBlock {
                        return Err(e.into());
                    }
                }
            },
            Ok(false) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                tracing::warn!("crossterm event::poll() error: {e}");
                if e.kind() != io::ErrorKind::WouldBlock {
                    return Err(e.into());
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

// ── Mouse selection ────────────────────────────────────────────────────

fn handle_mouse(app: &mut TuiApp, mouse: &crossterm::event::MouseEvent) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Start a new selection if clicking in the chat area.
            if let Some(pos) = app.screen_to_text(mouse.column, mouse.row) {
                app.clear_selection();
                app.selection_start = Some(pos);
                app.selection_end = Some(pos);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            // Extend selection while dragging.
            if app.selection_start.is_some()
                && let Some(pos) = app.screen_to_text_clamped(mouse.column, mouse.row)
            {
                app.selection_end = Some(pos);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // Finalize selection and copy to clipboard.
            if let Some(text) = app
                .normalized_selection()
                .map(|_| app.extract_selected_text())
            {
                if !text.is_empty() {
                    match clipboard::copy_to_clipboard(&text) {
                        Ok(lease) => {
                            app.clipboard_lease = Some(lease);
                            let bytes = text.len();
                            app.status_msg = format!("✓ Copied {bytes} bytes to clipboard");
                        }
                        Err(e) => {
                            app.status_msg = format!("✗ Copy failed: {e}");
                        }
                    }
                } else {
                    app.status_msg = "No text selected".to_string();
                    app.clear_selection();
                }
            } else {
                app.clear_selection();
            }
        }
        MouseEventKind::ScrollUp => {
            app.clear_selection();
            app.chat_scroll = app.chat_scroll.saturating_add(10);
        }
        MouseEventKind::ScrollDown => {
            app.clear_selection();
            app.chat_scroll = app.chat_scroll.saturating_sub(10);
        }
        _ => {}
    }
}

// ── State machine dispatch ────────────────────────────────────────────

/// Determine which mode the app is in and dispatch to the appropriate handler.
async fn handle_key(
    app: &mut TuiApp,
    key: KeyEvent,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) {
    // ── Approval mode takes priority over everything ──
    if let Some(pending_handle) = app.pending_approval.clone() {
        // Check if there's a pending approval
        let has_pending = {
            let guard = pending_handle.try_lock();
            guard.as_ref().map(|g| g.is_some()).unwrap_or(false)
        };
        if has_pending && handle_approval_key(app, &pending_handle, &key).await {
            return;
        }
    }

    if !app.suggestions.is_empty() {
        // Command palette mode: palette consumes Tab/Enter/Esc; everything
        // else falls through to normal input handling.
        if handle_command_palette_key(app, &key) {
            return;
        }
    }

    if let Some(result) = handle_global_shortcuts(app, &key)
        && result
    {
        return;
    }
    handle_normal_key(app, &key, agent, agent_tx).await;
}

// ── Command palette ────────────────────────────────────────────────────────
/// Returns `true` if the key was consumed; `false` means the caller
/// should fall through to normal text-editing handling.
fn handle_command_palette_key(app: &mut TuiApp, key: &KeyEvent) -> bool {
    const MAX_VISIBLE: usize = 8;
    match key.code {
        KeyCode::Tab | KeyCode::Down => {
            app.selected_suggestion = (app.selected_suggestion + 1) % app.suggestions.len();
            if app.selected_suggestion >= app.suggestion_scroll + MAX_VISIBLE {
                app.suggestion_scroll = app.selected_suggestion - MAX_VISIBLE + 1;
            }
            true
        }
        KeyCode::BackTab | KeyCode::Up => {
            if app.selected_suggestion > 0 {
                app.selected_suggestion -= 1;
            } else {
                app.selected_suggestion = app.suggestions.len() - 1;
                app.suggestion_scroll = app.suggestions.len().saturating_sub(MAX_VISIBLE);
            }
            if app.selected_suggestion < app.suggestion_scroll {
                app.suggestion_scroll = app.selected_suggestion;
            }
            true
        }
        KeyCode::Enter => {
            // If the current input exactly matches a command name, clear
            // suggestions so the Enter falls through to normal execution
            // instead of requiring a second press.
            let input_trimmed = app.input.trim().to_lowercase();
            let exact_match = app
                .suggestions
                .iter()
                .any(|c| c.slash_name() == input_trimmed);
            if exact_match {
                app.suggestions.clear();
                return false; // fall through → Normal → handle_enter
            }
            // Otherwise, accept the selected suggestion into the input.
            if let Some(cmd) = app.suggestions.get(app.selected_suggestion) {
                app.input = format!("{} ", cmd.slash_name());
                app.cursor = app.input.len();
            }
            app.suggestions.clear();
            true
        }
        KeyCode::Esc => {
            app.suggestions.clear();
            true
        }
        _ => false,
    }
}

// ── Global shortcuts ──────────────────────────────────────────────────

/// Returns true if the key was handled (shortcut consumed).
fn handle_global_shortcuts(app: &mut TuiApp, key: &KeyEvent) -> Option<bool> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }

    match key.code {
        KeyCode::Char('c') | KeyCode::Char('q') => {
            app.should_quit = true;
            Some(true)
        }
        KeyCode::Char('b') => {
            app.sidebar_visible = !app.sidebar_visible;
            Some(true)
        }
        KeyCode::Char('l') => {
            app.messages.clear();
            app.chat_scroll = 0;
            app.clear_selection();
            Some(true)
        }
        KeyCode::Char('y') => {
            match app.last_assistant_response() {
                Some(text) => {
                    let text = text.to_string();
                    match clipboard::copy_to_clipboard(&text) {
                        Ok(lease) => {
                            app.clipboard_lease = Some(lease);
                            let len = text.len();
                            app.status_msg =
                                format!("✓ Copied response to clipboard ({len} bytes)");
                        }
                        Err(e) => {
                            app.status_msg = format!("✗ Copy failed: {e}");
                        }
                    }
                }
                None => {
                    app.status_msg = "No response to copy".to_string();
                }
            }
            Some(true)
        }
        _ => None,
    }
}

// ── Normal mode input handling ────────────────────────────────────────

async fn handle_normal_key(
    app: &mut TuiApp,
    key: &KeyEvent,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) {
    // Shift+Enter: newline
    if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::Enter {
        app.input.insert(app.cursor, '\n');
        app.cursor += 1;
        return;
    }

    match key.code {
        KeyCode::Enter => handle_enter(app, agent, agent_tx).await,
        KeyCode::Char(c) => handle_char_input(app, c),
        KeyCode::Backspace => handle_backspace(app),
        KeyCode::Delete => handle_delete(app),
        KeyCode::Left => handle_cursor_left(app),
        KeyCode::Right => handle_cursor_right(app),
        KeyCode::Home => app.cursor = 0,
        KeyCode::End => app.cursor = app.input.len(),
        KeyCode::Up => handle_up(app, key),
        KeyCode::Down => handle_down(app, key),
        KeyCode::PageUp => app.chat_scroll = app.chat_scroll.saturating_add(30),
        KeyCode::PageDown => app.chat_scroll = app.chat_scroll.saturating_sub(30),
        KeyCode::Tab => app.sidebar_tab = (app.sidebar_tab + 1) % 3,
        KeyCode::Esc => handle_esc(app),
        _ => {}
    }
}

async fn handle_enter(
    app: &mut TuiApp,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) {
    if app.is_processing {
        // Can't send while processing; insert newline instead.
        app.input.insert(app.cursor, '\n');
        app.cursor += 1;
    } else if let Some(text) = app.submit_input() {
        if text.starts_with('/') {
            handle_slash_command(app, agent, &text).await;
        } else {
            // B5.3: drain staged attachments (/attach). If any, build a
            // multimodal Message and pass it to drive_chat; also bind them on
            // ChatResources so any autonomous run the agent spins up sees them.
            let staged = std::mem::take(&mut app.pending_attachments);
            let multimodal = if staged.is_empty() {
                None
            } else {
                match echo_agent_app_core::attachments::build_message_from_refs(&text, &staged) {
                    Ok(msg) => Some(msg),
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to build multimodal message; sending text only");
                        None
                    }
                }
            };
            let cancel = echo_agent::agent::CancellationToken::new();
            let sink: std::sync::Arc<dyn echo_agent_app_core::chat_driver::ChatSink> =
                std::sync::Arc::new(TuiChatSink::new(agent_tx.clone()));
            // B4.3 (spec §8): per-turn mode hint, mirroring GUI's chat.rs —
            // Chat → forbid create_complex_task; Task → lean towards it;
            // Auto → no hint (pure agent autonomy). Pure prompt, no code route.
            let mode_hint = match app.interaction_mode {
                InteractionMode::Chat => Some(
                    "Chat — this is a simple conversation. Do NOT call \
                     create_complex_task; reply directly in this turn."
                        .to_string(),
                ),
                InteractionMode::Task => Some(
                    "Task — the user expects involved work. When the request is \
                     genuinely complex (multi-step / multi-file / long research), \
                     prefer create_complex_task over a single inline reply."
                        .to_string(),
                ),
                InteractionMode::Auto => None,
            };
            let res = std::sync::Arc::new(echo_agent_app_core::chat_resources::ChatResources {
                pool: app.pool.clone(),
                store: app.task_runtime_store.clone(),
                sink,
                // TUI/GUI parity (AGENTS.md): bind this turn to the session's
                // conversation id so TaskRuntime runs + transcript projection work.
                conv_id: app.conversation_id.clone(),
                root_message_id: uuid::Uuid::new_v4().to_string(),
                // Bind staged refs so workers in an autonomous run see them too.
                attachments: staged,
                cancel,
                mode_hint,
                // B5.1 (TUI/GUI parity): build a layer_manager per turn from
                // review_integration so autonomous runs block-write their
                // completion memory (`taskrun:completed`). None = no review/memory
                // subsystem (writes become no-ops).
                layer_manager: app
                    .review_integration
                    .as_ref()
                    .map(|ri| std::sync::Arc::new(ri.create_layer_manager())),
            });
            send_to_agent(agent, text, multimodal, res).await;
        }
    }
}

fn handle_char_input(app: &mut TuiApp, c: char) {
    app.input.insert(app.cursor, c);
    app.cursor += c.len_utf8();
    app.update_suggestions();
}

fn handle_backspace(app: &mut TuiApp) {
    if app.cursor > 0 {
        let prev = app.input[..app.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        app.input.drain(prev..app.cursor);
        app.cursor = prev;
        app.update_suggestions();
    }
}

fn handle_delete(app: &mut TuiApp) {
    if app.cursor < app.input.len() {
        let cur = app.cursor;
        let next = app.input[cur..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| cur + i)
            .unwrap_or(app.input.len());
        app.input.drain(cur..next);
    }
}

fn handle_cursor_left(app: &mut TuiApp) {
    if app.cursor > 0 {
        let prev = app.input[..app.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        app.cursor = prev;
    }
}

fn handle_cursor_right(app: &mut TuiApp) {
    if app.cursor < app.input.len() {
        let cur = app.cursor;
        let ch_len = app.input[cur..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        app.cursor = (cur + ch_len).min(app.input.len());
    }
}

fn handle_up(app: &mut TuiApp, key: &KeyEvent) {
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        app.chat_scroll = app.chat_scroll.saturating_add(10);
    } else {
        app.history_up();
    }
}

fn handle_down(app: &mut TuiApp, key: &KeyEvent) {
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        app.chat_scroll = app.chat_scroll.saturating_sub(10);
    } else {
        app.history_down();
    }
}

fn handle_esc(app: &mut TuiApp) {
    if app.is_processing {
        app.is_processing = false;
        app.streaming_text.clear();
        app.pending_stream.clear();
        app.status_msg = "Cancelled".to_string();
    }
}

// ── Agent communication ─────────────────────────────────────────────────

/// Send a message to the agent using streaming and forward events to the UI.
/// `ChatSink` for the TUI: converts framework `AgentEvent`s into the TUI's
/// simplified local `AgentEvent` and forwards them to the UI render loop.
///
/// TUI/GUI parity (AGENTS.md): this is the TUI's renderer for the shared
/// `drive_chat` stream — the equivalent of GUI's `agent_event_to_chat_event`.
struct TuiChatSink {
    tx: mpsc::UnboundedSender<AgentEvent>,
}

impl TuiChatSink {
    fn new(tx: mpsc::UnboundedSender<AgentEvent>) -> Self {
        Self { tx }
    }
}

impl echo_agent_app_core::chat_driver::ChatSink for TuiChatSink {
    fn on_agent_event(&self, event: echo_agent::agent::AgentEvent) -> bool {
        let mapped = match event {
            echo_agent::agent::AgentEvent::Token(t) => AgentEvent::Token(t),
            echo_agent::agent::AgentEvent::ThinkStart => AgentEvent::ThinkStart,
            echo_agent::agent::AgentEvent::ThinkEnd {
                prompt_tokens,
                completion_tokens,
            } => AgentEvent::ThinkEnd {
                prompt_tokens,
                completion_tokens,
            },
            echo_agent::agent::AgentEvent::ToolBatchStart { tool_count } => {
                AgentEvent::ToolBatchStart { tool_count }
            }
            echo_agent::agent::AgentEvent::ToolBatchEnd => AgentEvent::ToolBatchEnd,
            echo_agent::agent::AgentEvent::FinalAnswer(answer) => AgentEvent::FinalAnswer(answer),
            echo_agent::agent::AgentEvent::ToolCall { name, args } => AgentEvent::ToolCall {
                name,
                args: args.to_string(),
            },
            echo_agent::agent::AgentEvent::ToolResult { name, output } => {
                AgentEvent::ToolResult { name, output }
            }
            echo_agent::agent::AgentEvent::ContextCompressed {
                before_count,
                after_count,
                before_tokens,
                after_tokens,
            } => AgentEvent::ContextCompressed {
                before_count,
                after_count,
                before_tokens,
                after_tokens,
            },
            echo_agent::agent::AgentEvent::Error { message, .. } => AgentEvent::Error(message),
            // LlmUsage: not rendered in TUI (token totals come from ThinkEnd),
            // but trace cache stats so they're not silently dropped (TUI/GUI
            // observability parity). Full usage rendering is a follow-up.
            echo_agent::agent::AgentEvent::LlmUsage {
                prompt_tokens,
                cached_prompt_tokens,
                cache_creation_prompt_tokens,
                usage_reported,
                ..
            } => {
                tracing::debug!(
                    prompt_tokens,
                    cached_prompt_tokens,
                    cache_creation_prompt_tokens,
                    usage_reported,
                    "TUI: LLM usage reported (cache stats; not rendered)"
                );
                return true;
            }
            // Other framework events (GuardTriggered, MemoryRecalled,
            // SafetyNotice, ParameterError, …) have no TUI rendering yet.
            other => {
                tracing::debug!(event = ?other, "TUI: unrendered agent event");
                return true;
            }
        };
        // If the UI dropped the receiver, stop streaming.
        self.tx.send(mapped).is_ok()
    }
}

async fn send_to_agent(
    agent: &AgentHandle,
    text: String,
    multimodal: Option<echo_agent::prelude::Message>,
    res: std::sync::Arc<echo_agent_app_core::chat_resources::ChatResources>,
) {
    use echo_agent_app_core::chat_driver::drive_chat;

    // 极简入口(Phase B1/B3):TUI 不预判 normal/complex——agent 自主决定是否
    // 建后台 Run(create_complex_task 工具,B3b)。ChatResources(pool/store/sink)
    // 经 drive_chat scope 进 task_local 供工具读。B5.3: multimodal 透传 /attach
    // 暂存的图片/文档(与 GUI 同路径)。
    let agent_owned = agent.clone();

    tokio::spawn(async move {
        if let Err(e) = drive_chat(&agent_owned, &text, multimodal.as_ref(), res).await {
            tracing::warn!(error = %e, "TUI drive_chat failed");
        }
    });
}

// ── Slash command handling ────────────────────────────────────────────

/// Stage a local file as an attachment for the next TUI message (B5.3).
///
/// Reads `path`, infers a MIME type from the extension, copies the file into
/// the global uploads dir (`~/.echo-agent/uploads/`, since the TUI has no
/// workspace concept), and appends an [`AttachmentRef`] to `out`. The caller
/// (`handle_enter`) rebuilds a multimodal `Message` from the refs and passes it
/// to `drive_chat`. Returns the display name + inferred MIME on success.
fn stage_attachment(
    out: &mut Vec<echo_agent_app_core::attachments::AttachmentRef>,
    path: &std::path::Path,
) -> std::io::Result<(String, String)> {
    use echo_agent_app_core::attachments::{AttachmentRef, resolve_uploads_dir};
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no filename")
        })?;
    let bytes = std::fs::read(path)?;
    let mime = infer_mime(&name);
    // Persist under the global uploads dir so the ref's path stays valid for
    // workers that re-read it later (matches the GUI's per-workspace uploads,
    // just global here).
    let uploads_dir = resolve_uploads_dir(None);
    std::fs::create_dir_all(&uploads_dir)?;
    let file_name = format!("{}_{}", uuid::Uuid::new_v4(), name);
    let dest = uploads_dir.join(file_name);
    std::fs::write(&dest, &bytes)?;
    out.push(AttachmentRef {
        path: dest,
        name: name.clone(),
        mime_type: mime.clone(),
    });
    Ok((name, mime))
}

/// Infer a MIME type from a filename extension (B5.3 TUI /attach). Defaults to
/// `application/octet-stream` for unknown extensions. Image MIMEs route to
/// `ContentPart::ImageUrl`; everything else to `ContentPart::File`.
fn infer_mime(name: &str) -> String {
    let ext = name.rsplit('.').next().map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") => "image/png".to_string(),
        Some("jpg") | Some("jpeg") => "image/jpeg".to_string(),
        Some("gif") => "image/gif".to_string(),
        Some("webp") => "image/webp".to_string(),
        Some("svg") => "image/svg+xml".to_string(),
        Some("pdf") => "application/pdf".to_string(),
        Some("txt") | Some("md") | Some("rs") | Some("py") | Some("ts") | Some("js")
        | Some("json") | Some("toml") | Some("yaml") | Some("yml") => "text/plain".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// Handle slash commands locally in the TUI.
async fn handle_slash_command(app: &mut TuiApp, agent: &AgentHandle, cmd: &str) {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let command = parts[0].to_lowercase();
    let args = parts.get(1).unwrap_or(&"");

    let slash_cmd = command
        .strip_prefix('/')
        .and_then(|name| name.parse::<SlashCommand>().ok());

    match slash_cmd {
        Some(SlashCommand::Help) => {
            let mut help = String::from("Available commands:\n\n");
            for (cat, cmds) in SlashCommand::grouped() {
                help.push_str(&format!("  {} {}\n", cat.icon(), cat.label()));
                for c in cmds {
                    let usage = c.usage();
                    help.push_str(&format!(
                        "    {:<18} {}{}\n",
                        c.slash_name(),
                        c.description(),
                        if usage.is_empty() {
                            String::new()
                        } else {
                            format!("  {}", usage)
                        }
                    ));
                }
                help.push('\n');
            }
            help.push_str("  Keybindings:\n");
            help.push_str("    Ctrl+C / Ctrl+Q    Quit\n");
            help.push_str("    Ctrl+B             Toggle sidebar\n");
            help.push_str("    Ctrl+L             Clear chat\n");
            help.push_str("    Shift+Enter        Newline in input\n");
            help.push_str("    Esc                Cancel generation\n");
            help.push_str("    Tab                Cycle sidebar tabs\n");
            help.push_str("    Up/Down            Navigate input history\n");
            help.push_str("    Shift+Up/Down      Scroll chat\n");
            help.push_str("    PageUp/PageDown    Scroll chat faster\n");
            help.push_str("    Mouse wheel        Scroll chat\n");

            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: help,
            });
        }
        Some(SlashCommand::Model) => {
            if args.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Current model: {}", app.model),
                });
            } else {
                app.model = args.to_string();
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Model switched to: {}", app.model),
                });
            }
        }
        Some(SlashCommand::Reset) => {
            app.messages.clear();
            app.tokens = (0, 0, 0);
            agent
                .read_async(|a| {
                    Box::pin(async move {
                        use echo_agent::agent::Agent;
                        a.reset().await;
                    })
                })
                .await;
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: "Conversation reset.".to_string(),
            });
        }
        Some(SlashCommand::Stats) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "Session stats:\n  Model: {}\n  Mode: {}\n  Messages: {}\n  Tokens: {}/{}/{} (prompt/completion/total)\n  Tools: {}",
                    app.model,
                    app.mode,
                    app.messages.len(),
                    app.tokens.0,
                    app.tokens.1,
                    app.tokens.2,
                    app.tool_count
                ),

            });
        }
        Some(SlashCommand::Compact) => {
            let result = agent
                .write_async(|a| Box::pin(async move { a.force_compress_context().await }))
                .await;
            match result {
                Ok((stats, _checkpoint)) => {
                    let saved = stats.before_tokens.saturating_sub(stats.after_tokens);
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!(
                            "上下文已压缩: {} → {} 条消息, 节省 ≈{} tokens",
                            stats.before_count, stats.after_count, saved
                        ),
                    });
                }
                Err(e) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("压缩失败: {e}"),
                    });
                }
            }
        }
        Some(SlashCommand::Copy) => match app.last_assistant_response() {
            Some(text) => {
                let text = text.to_string();
                match clipboard::copy_to_clipboard(&text) {
                    Ok(lease) => {
                        app.clipboard_lease = Some(lease);
                        let len = text.len();
                        app.status_msg = format!("✓ Copied response to clipboard ({len} bytes)");
                    }
                    Err(e) => {
                        app.status_msg = format!("✗ Copy failed: {e}");
                    }
                }
            }
            None => {
                app.status_msg = "No response to copy".to_string();
            }
        },
        Some(SlashCommand::Plan) => {
            app.mode = "plan".to_string();
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: "Entered plan mode. Write operations are disabled.".to_string(),
            });
        }
        Some(SlashCommand::Mode) => {
            // Manual routing override for the next message (TUI/GUI parity with
            // `set_interaction_mode`). Auto = router decides; Chat = force normal
            // chat; Task = force TaskRuntime. Updates the status-bar label too.
            if args.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!(
                        "Interaction mode: {} (auto/chat/task)",
                        app.interaction_mode.label()
                    ),
                });
            } else {
                match parse_interaction_mode(args) {
                    Some(m) => {
                        app.interaction_mode = m;
                        app.mode = m.label().to_string();
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!("Interaction mode set to: {}", m.label()),
                        });
                    }
                    None => {
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!("Unknown mode '{}'; use auto, chat, or task", args),
                        });
                    }
                }
            }
        }
        Some(SlashCommand::Attach) => {
            // B5.3: stage a file (image/document) for the next message. Reads
            // the file, persists it under the global uploads dir, and pushes an
            // AttachmentRef onto pending_attachments. The next Enter sends it
            // alongside the typed text via drive_chat(multimodal=Some), then
            // drains the buffer. TUI has no workspace concept, so use the
            // global ~/.echo-agent/uploads/ dir.
            if args.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Usage: /attach <path>  (stage a file for the next message)"
                        .to_string(),
                });
            } else {
                let path = std::path::PathBuf::from(args.trim());
                match stage_attachment(&mut app.pending_attachments, &path) {
                    Ok((name, mime)) => {
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!(
                                "Attached: {} ({}). It will be sent with your next message.{}",
                                name,
                                mime,
                                if app.pending_attachments.len() > 1 {
                                    format!("\n{} file(s) staged.", app.pending_attachments.len())
                                } else {
                                    String::new()
                                }
                            ),
                        });
                    }
                    Err(e) => {
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!("Failed to attach '{}': {e}", path.display()),
                        });
                    }
                }
            }
        }
        Some(SlashCommand::Permission) => {
            if args.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Permission mode: {}", app.permission_mode),
                });
            } else {
                app.permission_mode = args.to_string();
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Permission mode set to: {}", app.permission_mode),
                });
            }
        }
        Some(SlashCommand::Quit) | Some(SlashCommand::Exit) => {
            app.should_quit = true;
        }
        Some(SlashCommand::MemoryReview) => {
            // Create ReviewIntegration on-the-fly from the agent's store
            let store = agent.read(|a| a.store().cloned()).await;
            match store {
                Some(store) => {
                    let echo_agent_dir = echo_agent_app_core::evolution::discover_echo_agent_dir();
                    let review_integration = echo_agent_app_core::evolution::ReviewIntegration::new(
                        echo_agent::evolution::ReviewConfig::default(),
                        echo_agent_dir,
                        store,
                    );

                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: "📋 Running memory review...".to_string(),
                    });

                    match review_integration.run_review().await {
                        Ok(report) => {
                            let formatted =
                                echo_agent_app_core::evolution::format_review_report(&report);
                            app.messages.push(ChatMessage {
                                role: MessageRole::System,
                                content: formatted,
                            });
                        }
                        Err(e) => {
                            app.messages.push(ChatMessage {
                                role: MessageRole::System,
                                content: format!("Memory review failed: {e}"),
                            });
                        }
                    }
                }
                None => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: "No memory store configured. Cannot run memory review."
                            .to_string(),
                    });
                }
            }
        }
        Some(SlashCommand::SkillCandidates) => {
            // List candidates and drafts from Curator state
            let curator = echo_agent::improve::Curator::default_path(
                echo_agent::improve::CuratorConfig::default(),
            );
            let state = curator.load_state();
            let items: Vec<_> = state
                .skills
                .iter()
                .filter(|(_, m)| {
                    matches!(
                        m.lifecycle,
                        echo_agent::improve::SkillLifecycle::Candidate
                            | echo_agent::improve::SkillLifecycle::Draft
                    )
                })
                .collect();

            if items.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No skill candidates or drafts found. Run /memory-review to detect patterns.".to_string(),
                });
            } else {
                let mut content = String::from("🎯 Skill Candidates & Drafts:\n");
                for (name, meta) in &items {
                    let icon = match meta.lifecycle {
                        echo_agent::improve::SkillLifecycle::Candidate => "🎯",
                        echo_agent::improve::SkillLifecycle::Draft => "📝",
                        _ => "  ",
                    };
                    content.push_str(&format!("  {} {} [{:?}]\n", icon, name, meta.lifecycle));
                }
                content.push_str("\nUse /skill-create <name> to generate a draft, /skill-promote <name> to activate.");
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content,
                });
            }
        }
        Some(SlashCommand::Status) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "Status: model={}, mode={}, processing={}, messages={}, tools={}",
                    app.model,
                    app.mode,
                    app.is_processing,
                    app.messages.len(),
                    app.tool_count
                ),
            });
        }
        Some(SlashCommand::Cost) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "Token usage: prompt={}, completion={}, total={}",
                    app.tokens.0, app.tokens.1, app.tokens.2
                ),
            });
        }
        Some(SlashCommand::Tools) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Available tools: {} (see sidebar for list)", app.tool_count),
            });
        }
        _ => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Command '{}' sent to agent for processing.", command),
            });
        }
    }

    app.is_processing = false;
    app.status_msg = "Ready".to_string();
}

// ── Parallel task progress strip ────────────────────────────────────────

/// Format elapsed time from an Instant to a human-readable label like "2m 8s".
fn format_elapsed(start: Instant) -> String {
    let secs = start.elapsed().as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// Process a TaskEvent and update `app.parallel_tasks` accordingly.
///
/// - `Created` → add a new entry with Pending status
/// - `Updated` → update status (InProgress/Completed/Failed/Cancelled)
/// - `Progress` → update percentage, phase, message
/// - `Completed` → mark as Completed
/// - `Failed` → mark as Failed
fn update_parallel_tasks(app: &mut TuiApp, event: &TaskEvent) {
    match event {
        TaskEvent::Created { task } => {
            // Only show if not already present
            if app.parallel_tasks.iter().any(|e| e.task_id == task.id) {
                return;
            }
            let name = if task.subject.is_empty() {
                task.description.clone()
            } else {
                task.subject.clone()
            };
            app.parallel_tasks.push(TaskProgressEntry {
                task_id: task.id.clone(),
                name: crate::tui::widgets::task_strip::truncate_str(&name, 30),
                status: TaskStripStatus::Pending,
                progress_pct: 0.0,
                phase: String::new(),
                message: None,
                started_at: Instant::now(),
                elapsed_label: "0s".to_string(),
            });
        }

        TaskEvent::Updated {
            task_id,
            new_status,
            ..
        } => {
            if let Some(entry) = app
                .parallel_tasks
                .iter_mut()
                .find(|e| e.task_id == *task_id)
            {
                use echo_agent::tasks::TaskStatus;
                entry.status = match new_status {
                    TaskStatus::InProgress => TaskStripStatus::Running,
                    TaskStatus::Completed => TaskStripStatus::Completed,
                    TaskStatus::Cancelled => TaskStripStatus::Cancelled,
                    TaskStatus::Failed(e) => TaskStripStatus::Failed(e.clone()),
                    TaskStatus::TimedOut { error } => {
                        TaskStripStatus::Failed(format!("Timeout: {error}"))
                    }
                    _ => entry.status.clone(),
                };
                entry.elapsed_label = format_elapsed(entry.started_at);
            }
        }

        TaskEvent::Progress { task_id, progress } => {
            if let Some(entry) = app
                .parallel_tasks
                .iter_mut()
                .find(|e| e.task_id == *task_id)
            {
                entry.progress_pct = progress.percentage;
                entry.phase = progress.current_phase.clone();
                entry.message = progress.message.clone();
                entry.elapsed_label = format_elapsed(entry.started_at);
                // If we get progress, it's definitely running
                if entry.status == TaskStripStatus::Pending {
                    entry.status = TaskStripStatus::Running;
                }
            }
        }

        TaskEvent::Completed { task_id, .. } => {
            if let Some(entry) = app
                .parallel_tasks
                .iter_mut()
                .find(|e| e.task_id == *task_id)
            {
                entry.status = TaskStripStatus::Completed;
                entry.progress_pct = 100.0;
                entry.elapsed_label = format_elapsed(entry.started_at);
            }
        }

        TaskEvent::Failed { task_id, error, .. } => {
            if let Some(entry) = app
                .parallel_tasks
                .iter_mut()
                .find(|e| e.task_id == *task_id)
            {
                entry.status = TaskStripStatus::Failed(error.clone());
                entry.elapsed_label = format_elapsed(entry.started_at);
            }
        }

        _ => {} // Deleted, Assigned — ignore
    }

    // Prune completed/failed entries older than 30 seconds to keep the strip clean.
    app.parallel_tasks.retain(|e| match e.status {
        TaskStripStatus::Completed | TaskStripStatus::Cancelled => {
            e.started_at.elapsed().as_secs() < 30
        }
        TaskStripStatus::Failed(_) => e.started_at.elapsed().as_secs() < 60,
        _ => true,
    });
}

/// Parse a user-supplied interaction-mode argument (`auto` / `chat` / `task`,
/// case-insensitive) for the `/mode` command. Returns `None` for unknown or
/// empty input so the caller can surface an error instead of silently
/// falling back to `Auto`.
fn parse_interaction_mode(
    arg: &str,
) -> Option<echo_agent_app_core::tasks::task_runtime::types::InteractionMode> {
    use echo_agent_app_core::tasks::task_runtime::types::InteractionMode;
    match arg.trim().to_lowercase().as_str() {
        "auto" => Some(InteractionMode::Auto),
        "chat" => Some(InteractionMode::Chat),
        "task" => Some(InteractionMode::Task),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_interaction_mode;
    use echo_agent_app_core::tasks::task_runtime::types::InteractionMode;

    #[test]
    fn parses_auto_chat_task_case_insensitively() {
        assert_eq!(parse_interaction_mode("auto"), Some(InteractionMode::Auto));
        assert_eq!(parse_interaction_mode("chat"), Some(InteractionMode::Chat));
        assert_eq!(parse_interaction_mode("task"), Some(InteractionMode::Task));
        // Case-insensitive.
        assert_eq!(parse_interaction_mode("Chat"), Some(InteractionMode::Chat));
        assert_eq!(parse_interaction_mode("TASK"), Some(InteractionMode::Task));
        // Surrounding whitespace is tolerated (e.g. `/mode  chat`).
        assert_eq!(
            parse_interaction_mode(" chat "),
            Some(InteractionMode::Chat)
        );
    }

    #[test]
    fn rejects_unknown_and_empty() {
        assert_eq!(parse_interaction_mode("plan"), None);
        assert_eq!(parse_interaction_mode("xyz"), None);
        assert_eq!(parse_interaction_mode(""), None);
    }
}
