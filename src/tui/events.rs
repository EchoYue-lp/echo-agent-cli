//! TUI event loop — handles keyboard input, terminal resize, and agent streaming.

use super::{ChatMessage, MessageRole, TuiApp};
use crate::agent_handle::AgentHandle;
use crate::tui::commands::SlashCommand;
use crate::tui::ui;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

/// Poll interval for non-blocking event check.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

enum AgentEvent {
    Response(String),
    Error(String),
}

/// Run the main event loop.
pub async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
    agent: AgentHandle,
) -> anyhow::Result<()> {
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();

    loop {
        // Draw UI.
        terminal.draw(|f| ui::draw(f, app))?;

        while let Ok(event) = agent_rx.try_recv() {
            match event {
                AgentEvent::Response(response) => {
                    app.append_stream(&response);
                    app.finalize_stream();
                }
                AgentEvent::Error(e) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Error: {e}"),
                        tool_calls: vec![],
                    });
                    app.is_processing = false;
                    app.status_msg = "Error".to_string();
                }
            }
        }

        // Handle events.
        if event::poll(POLL_INTERVAL)? {
            match event::read()? {
                Event::Key(key) => handle_key(app, key, &agent, agent_tx.clone()).await,
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        app.chat_scroll = app.chat_scroll.saturating_add(10)
                    }
                    MouseEventKind::ScrollDown => {
                        app.chat_scroll = app.chat_scroll.saturating_sub(10)
                    }
                    _ => {}
                },
                Event::Resize(_, _) => {} // ratatui handles resize automatically
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

// ── State machine dispatch ────────────────────────────────────────────

/// Determine which mode the app is in and dispatch to the appropriate handler.
async fn handle_key(
    app: &mut TuiApp,
    key: KeyEvent,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) {
    // Determine active mode based on app state
    let mode = if app.picker.is_some() {
        AppMode::Picker
    } else if app.diff_popup.is_some() {
        AppMode::DiffPopup
    } else if app.approval.is_some() {
        AppMode::Approval
    } else if !app.suggestions.is_empty() {
        AppMode::CommandPalette
    } else {
        AppMode::Normal
    };

    match mode {
        AppMode::Picker => handle_picker_key(app, &key),
        AppMode::DiffPopup => handle_diff_popup_key(app, &key),
        AppMode::Approval => handle_approval_key(app, &key),
        AppMode::CommandPalette => handle_command_palette_key(app, &key),
        AppMode::Normal => {
            if let Some(result) = handle_global_shortcuts(app, &key) {
                if result {
                    return;
                }
            }
            handle_normal_key(app, &key, agent, agent_tx).await;
        }
    }
}

/// Application interaction modes for state-machine dispatch.
enum AppMode {
    Normal,
    Picker,
    DiffPopup,
    Approval,
    CommandPalette,
}

// ── Mode-specific handlers ──────────────────────────────────────────────

fn handle_picker_key(app: &mut TuiApp, key: &KeyEvent) {
    let picker = match app.picker.as_mut() {
        Some(p) => p,
        None => return,
    };
    match key.code {
        KeyCode::Up => picker.move_up(),
        KeyCode::Down => picker.move_down(),
        KeyCode::Enter => {
            let value = picker.selected_value().map(|s| s.to_string());
            app.picker = None;
            if let Some(val) = value {
                app.status_msg = format!("Selected: {}", val);
            }
        }
        KeyCode::Esc => {
            app.picker = None;
        }
        _ => {}
    }
}

fn handle_diff_popup_key(app: &mut TuiApp, key: &KeyEvent) {
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        app.diff_popup = None;
    }
}

fn handle_approval_key(app: &mut TuiApp, key: &KeyEvent) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            if let Some(ref approval) = app.approval.take() {
                app.status_msg = format!("Approved: {}", approval.tool_name);
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            if let Some(ref approval) = app.approval.take() {
                app.status_msg = format!("Denied: {}", approval.tool_name);
            }
        }
        _ => {}
    }
}

fn handle_command_palette_key(app: &mut TuiApp, key: &KeyEvent) {
    const MAX_VISIBLE: usize = 8;
    match key.code {
        KeyCode::Tab | KeyCode::Down => {
            app.selected_suggestion = (app.selected_suggestion + 1) % app.suggestions.len();
            if app.selected_suggestion >= app.suggestion_scroll + MAX_VISIBLE {
                app.suggestion_scroll = app.selected_suggestion - MAX_VISIBLE + 1;
            }
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
        }
        KeyCode::Enter => {
            if let Some(cmd) = app.suggestions.get(app.selected_suggestion) {
                app.input = format!("{} ", cmd.slash_name());
                app.cursor = app.input.len();
            }
            app.suggestions.clear();
        }
        KeyCode::Esc => {
            app.suggestions.clear();
        }
        _ => {}
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
            send_to_agent(agent, text, agent_tx.clone()).await;
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
        app.status_msg = "Cancelled".to_string();
    }
}

// ── Agent communication ─────────────────────────────────────────────────

/// Send a message to the agent and handle the response without blocking the UI loop.
async fn send_to_agent(
    agent: &AgentHandle,
    text: String,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) {
    let agent_clone = agent.clone();
    tokio::spawn(async move {
        let text_clone = text.clone();
        let result: Result<String, String> = agent_clone
            .read_async(|a| {
                let task = text_clone.clone();
                Box::pin(async move {
                    use echo_agent::agent::Agent;
                    a.execute(&task).await.map_err(|e| e.to_string())
                })
            })
            .await;

        let _ = match result {
            Ok(response) => agent_tx.send(AgentEvent::Response(response)),
            Err(e) => agent_tx.send(AgentEvent::Error(e)),
        };
    });
}

// ── Slash command handling ────────────────────────────────────────────

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
                tool_calls: vec![],
            });
        }
        Some(SlashCommand::Mode) => {
            if args.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Current mode: {}", app.mode),
                    tool_calls: vec![],
                });
            } else {
                app.mode = args.to_string();
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Mode switched to: {}", app.mode),
                    tool_calls: vec![],
                });
            }
        }
        Some(SlashCommand::Model) => {
            if args.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Current model: {}", app.model),
                    tool_calls: vec![],
                });
            } else {
                app.model = args.to_string();
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Model switched to: {}", app.model),
                    tool_calls: vec![],
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
                tool_calls: vec![],
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
                tool_calls: vec![],
            });
        }
        Some(SlashCommand::Compact) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: "Context compression requested. (Will be wired to agent)".to_string(),
                tool_calls: vec![],
            });
        }
        Some(SlashCommand::Plan) => {
            app.mode = "plan".to_string();
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: "Entered plan mode. Write operations are disabled.".to_string(),
                tool_calls: vec![],
            });
        }
        Some(SlashCommand::Permission) => {
            if args.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Permission mode: {}", app.permission_mode),
                    tool_calls: vec![],
                });
            } else {
                app.permission_mode = args.to_string();
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Permission mode set to: {}", app.permission_mode),
                    tool_calls: vec![],
                });
            }
        }
        Some(SlashCommand::Quit) | Some(SlashCommand::Exit) => {
            app.should_quit = true;
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
                tool_calls: vec![],
            });
        }
        Some(SlashCommand::Cost) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "Token usage: prompt={}, completion={}, total={}",
                    app.tokens.0, app.tokens.1, app.tokens.2
                ),
                tool_calls: vec![],
            });
        }
        Some(SlashCommand::Tools) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Available tools: {} (see sidebar for list)", app.tool_count),
                tool_calls: vec![],
            });
        }
        _ => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Command '{}' sent to agent for processing.", command),
                tool_calls: vec![],
            });
        }
    }

    app.is_processing = false;
    app.status_msg = "Ready".to_string();
}
