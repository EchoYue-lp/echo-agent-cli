//! TUI event loop — handles keyboard input, terminal resize, and agent streaming.

use super::{TuiApp, ChatMessage, MessageRole, ToolCallInfo, ToolCallStatus};
use crate::agent_handle::AgentHandle;
use crate::tui::ui;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::Duration;

/// Poll interval for non-blocking event check.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Run the main event loop.
pub async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
    agent: AgentHandle,
) -> anyhow::Result<()> {
    loop {
        // Draw UI
        terminal.draw(|f| ui::draw(f, app))?;

        // Handle events
        if event::poll(POLL_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                handle_key(app, key, &agent).await;
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Handle a keyboard event.
async fn handle_key(app: &mut TuiApp, key: KeyEvent, agent: &AgentHandle) {
    // Global shortcuts
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('q') => {
                app.should_quit = true;
                return;
            }
            KeyCode::Char('b') => {
                app.sidebar_visible = !app.sidebar_visible;
                return;
            }
            KeyCode::Char('l') => {
                // Clear chat
                app.messages.clear();
                app.chat_scroll = 0;
                return;
            }
            _ => {}
        }
    }

    // If diff popup is open, handle popup keys
    if app.diff_popup.is_some() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.diff_popup = None;
            }
            _ => {}
        }
        return;
    }

    // If approval is showing, handle approval keys
    if app.approval.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                // Approve
                if let Some(ref approval) = app.approval.take() {
                    app.status_msg = format!("Approved: {}", approval.tool_name);
                    // TODO: send approval to agent via HITL dispatcher
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                // Deny
                if let Some(ref approval) = app.approval.take() {
                    app.status_msg = format!("Denied: {}", approval.tool_name);
                }
            }
            _ => {}
        }
        return;
    }

    // If suggestions are showing
    if !app.suggestions.is_empty() {
        match key.code {
            KeyCode::Tab | KeyCode::Down => {
                app.selected_suggestion =
                    (app.selected_suggestion + 1) % app.suggestions.len();
                return;
            }
            KeyCode::BackTab | KeyCode::Up => {
                if app.selected_suggestion > 0 {
                    app.selected_suggestion -= 1;
                } else {
                    app.selected_suggestion = app.suggestions.len() - 1;
                }
                return;
            }
            KeyCode::Enter => {
                // Apply selected suggestion
                if let Some(cmd) = app.suggestions.get(app.selected_suggestion) {
                    app.input = format!("{} ", cmd);
                    app.cursor = app.input.len();
                }
                app.suggestions.clear();
                return;
            }
            KeyCode::Esc => {
                app.suggestions.clear();
                return;
            }
            _ => {}
        }
    }

    // Normal input handling
    match key.code {
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) || app.is_processing {
                // Shift+Enter: insert newline
                app.input.insert(app.cursor, '\n');
                app.cursor += 1;
            } else if let Some(text) = app.submit_input() {
                // Send message to agent
                send_to_agent(app, agent, text).await;
            }
        }
        KeyCode::Char(c) => {
            app.input.insert(app.cursor, c);
            app.cursor += 1;
            app.update_suggestions();
        }
        KeyCode::Backspace => {
            if app.cursor > 0 {
                app.cursor -= 1;
                app.input.remove(app.cursor);
                app.update_suggestions();
            }
        }
        KeyCode::Delete => {
            if app.cursor < app.input.len() {
                app.input.remove(app.cursor);
            }
        }
        KeyCode::Left => {
            if app.cursor > 0 {
                app.cursor -= 1;
            }
        }
        KeyCode::Right => {
            if app.cursor < app.input.len() {
                app.cursor += 1;
            }
        }
        KeyCode::Home => {
            app.cursor = 0;
        }
        KeyCode::End => {
            app.cursor = app.input.len();
        }
        KeyCode::Up => {
            app.history_up();
        }
        KeyCode::Down => {
            app.history_down();
        }
        KeyCode::PageUp => {
            app.chat_scroll = app.chat_scroll.saturating_add(5);
        }
        KeyCode::PageDown => {
            app.chat_scroll = app.chat_scroll.saturating_sub(5);
        }
        KeyCode::Tab => {
            // Cycle sidebar tabs
            app.sidebar_tab = (app.sidebar_tab + 1) % 3;
        }
        KeyCode::Esc => {
            if app.is_processing {
                // Cancel current generation
                app.is_processing = false;
                app.streaming_text.clear();
                app.status_msg = "Cancelled".to_string();
            }
        }
        _ => {}
    }
}

/// Send a message to the agent and handle the streaming response.
async fn send_to_agent(app: &mut TuiApp, agent: &AgentHandle, text: String) {
    // Handle slash commands locally first
    if text.starts_with('/') {
        handle_slash_command(app, &text);
        return;
    }

    // Send to agent via execute (uses Agent trait)
    let agent_clone = agent.clone();
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

    match result {
        Ok(response) => {
            app.append_stream(&response);
            app.finalize_stream();
        }
        Err(e) => {
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

/// Handle slash commands locally in the TUI.
fn handle_slash_command(app: &mut TuiApp, cmd: &str) {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let command = parts[0].to_lowercase();
    let args = parts.get(1).unwrap_or(&"");

    match command.as_str() {
        "/help" => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: r#"Available commands:
  /help          Show this help
  /mode <name>   Switch mode (general/coding/research/data/writing)
  /model <name>  Switch model
  /permission    Show/set permission mode
  /tools         List available tools
  /diff [file]   Show git diff or file diff
  /reset         Reset conversation
  /history       Show session history
  /stats         Show session statistics
  /compact       Compress context
  /plan          Enter plan mode (read-only)
  Ctrl+B         Toggle sidebar
  Ctrl+L         Clear chat
  Ctrl+C         Quit
  Esc            Cancel generation
  Tab            Cycle sidebar tabs
  Shift+Enter    Newline in input
  ↑/↓            Navigate input history"#
                    .to_string(),
                tool_calls: vec![],
            });
        }
        "/mode" => {
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
        "/model" => {
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
        "/reset" => {
            app.messages.clear();
            app.tokens = (0, 0, 0);
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: "Conversation reset.".to_string(),
                tool_calls: vec![],
            });
        }
        "/compact" => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: "Context compression requested. (Will be wired to agent)".to_string(),
                tool_calls: vec![],
            });
        }
        "/stats" => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "Session stats:\n  Model: {}\n  Mode: {}\n  Messages: {}\n  Tokens: {}/{}/{} (prompt/completion/total)\n  Tools: {}",
                    app.model, app.mode, app.messages.len(),
                    app.tokens.0, app.tokens.1, app.tokens.2,
                    app.tool_count
                ),
                tool_calls: vec![],
            });
        }
        "/plan" => {
            app.mode = "plan".to_string();
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: "Entered plan mode. Write operations are disabled.".to_string(),
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
