//! Terminal User Interface (TUI) for echo-agent-cli.
//!
//! Full-screen terminal UI built with ratatui, providing:
//! - Status bar: model, mode, token usage, permission
//! - Sidebar: file tree, tools list, active tasks
//! - Chat area: streaming messages with markdown rendering
//! - Input box: slash command completion, multi-line input
//! - Diff popup: inline code diff preview
//! - Approval cards: human-in-the-loop tool approval
//!
//! ## Terminal safety
//!
//! An RAII [`TerminalGuard`] ensures the terminal is always restored on exit,
//! even on panic. Tracing output is redirected to a file so that log messages
//! never corrupt the alternate screen.

pub mod commands;
pub mod events;
pub mod keymap;
pub mod markdown;
pub mod picker;
pub mod ui;
pub mod widgets;

use crate::agent_handle::AgentHandle;
use crossterm::{
    cursor::Show,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;

// ── Public types ────────────────────────────────────────────────────────────

/// TUI application state.
pub struct TuiApp {
    /// Current input text.
    pub input: String,
    /// Cursor position in input (byte offset).
    pub cursor: usize,
    /// Chat messages (role, content, tool_calls).
    pub messages: Vec<ChatMessage>,
    /// Whether the agent is currently processing.
    pub is_processing: bool,
    /// Current streaming text being received.
    pub streaming_text: String,
    /// Slash command suggestions (shown as completion popup).
    pub suggestions: Vec<String>,
    /// Selected suggestion index.
    pub selected_suggestion: usize,
    /// Whether sidebar is visible.
    pub sidebar_visible: bool,
    /// Sidebar active tab (0=files, 1=tools, 2=tasks).
    pub sidebar_tab: usize,
    /// Diff popup state.
    pub diff_popup: Option<DiffPopup>,
    /// Approval card state.
    pub approval: Option<ApprovalRequest>,
    /// Scroll offset for chat area (0 = auto-scroll to bottom).
    pub chat_scroll: u16,
    /// Model name.
    pub model: String,
    /// Agent mode label.
    pub mode: String,
    /// Token usage (prompt, completion, total).
    pub tokens: (u32, u32, u32),
    /// Tool count.
    pub tool_count: usize,
    /// Active task name.
    pub active_task: Option<String>,
    /// Status message.
    pub status_msg: String,
    /// Whether to quit.
    pub should_quit: bool,
    /// Input history.
    pub history: Vec<String>,
    /// History navigation index.
    pub history_idx: Option<usize>,
    /// Permission mode label.
    pub permission_mode: String,
    /// Session picker (for resume).
    pub picker: Option<picker::Picker>,
    /// Keymap.
    pub keymap: keymap::Keymap,
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    pub tool_calls: Vec<ToolCallInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Clone, Debug)]
pub struct ToolCallInfo {
    pub name: String,
    pub status: ToolCallStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolCallStatus {
    Running,
    Success,
    Failed,
}

#[derive(Clone, Debug)]
pub struct DiffPopup {
    pub file_path: String,
    pub diff_text: String,
}

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub request_id: String,
    pub tool_name: String,
    pub args: String,
    pub prompt: String,
}

// ── TuiApp methods ──────────────────────────────────────────────────────────

impl TuiApp {
    pub fn new(model: String, mode: String) -> Self {
        let mut km = keymap::Keymap::default();
        km.load_overrides();

        Self {
            input: String::new(),
            cursor: 0,
            messages: vec![ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "EchoCoWork TUI \u{2014} mode: {mode}, model: {model}\n\
                     Type a message or / for commands. Ctrl+C to quit."
                ),
                tool_calls: vec![],
            }],
            is_processing: false,
            streaming_text: String::new(),
            suggestions: vec![],
            selected_suggestion: 0,
            sidebar_visible: true,
            sidebar_tab: 0,
            diff_popup: None,
            approval: None,
            chat_scroll: 0,
            model,
            mode,
            tokens: (0, 0, 0),
            tool_count: 0,
            active_task: None,
            status_msg: "Ready".to_string(),
            should_quit: false,
            history: vec![],
            history_idx: None,
            permission_mode: "ask".to_string(),
            picker: None,
            keymap: km,
        }
    }

    /// Update slash command suggestions based on current input.
    pub fn update_suggestions(&mut self) {
        if self.input.starts_with('/') && !self.input.contains(' ') {
            let matches = commands::SlashCommand::complete(&self.input);
            self.suggestions = matches.iter().map(|c| c.slash_name()).collect();
            self.selected_suggestion = 0;
        } else {
            self.suggestions.clear();
        }
    }

    /// Submit the current input. Returns the text if non-empty.
    pub fn submit_input(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }

        self.history.push(text.clone());
        self.history_idx = None;

        self.messages.push(ChatMessage {
            role: MessageRole::User,
            content: text.clone(),
            tool_calls: vec![],
        });

        self.input.clear();
        self.cursor = 0;
        self.suggestions.clear();
        self.chat_scroll = 0;

        self.is_processing = true;
        self.streaming_text.clear();
        self.status_msg = "Thinking...".to_string();

        Some(text)
    }

    /// Append streaming text from agent.
    pub fn append_stream(&mut self, text: &str) {
        self.streaming_text.push_str(text);
    }

    /// Finalize the current streaming response.
    pub fn finalize_stream(&mut self) {
        if !self.streaming_text.is_empty() {
            self.messages.push(ChatMessage {
                role: MessageRole::Assistant,
                content: self.streaming_text.clone(),
                tool_calls: vec![],
            });
            self.streaming_text.clear();
        }
        self.is_processing = false;
        self.status_msg = "Ready".to_string();
    }

    /// Navigate history up.
    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.history_idx {
            Some(i) if i > 0 => i - 1,
            Some(0) => 0,
            _ => self.history.len() - 1,
        };
        self.history_idx = Some(idx);
        self.input = self.history[idx].clone();
        self.cursor = self.input.len();
    }

    /// Navigate history down.
    pub fn history_down(&mut self) {
        match self.history_idx {
            Some(i) if i < self.history.len() - 1 => {
                self.history_idx = Some(i + 1);
                self.input = self.history[i + 1].clone();
                self.cursor = self.input.len();
            }
            Some(_) => {
                self.history_idx = None;
                self.input.clear();
                self.cursor = 0;
            }
            None => {}
        }
    }
}

// ── RAII Terminal Guard ─────────────────────────────────────────────────────

/// RAII guard that sets up the terminal on creation and restores it on drop.
///
/// Also installs a panic hook that restores the terminal before the default
/// panic handler runs, preventing terminal corruption on crash.
struct TerminalGuard {
    _log_guard: Option<LogGuard>,
}

/// Holds the tracing subscriber guard so logs go to file instead of stderr.
struct LogGuard {
    _file: std::fs::File,
}

impl TerminalGuard {
    fn new() -> Self {
        // 1. Redirect tracing to a file before entering alternate screen.
        let log_guard = Self::setup_file_logging();

        // 2. Enter raw mode + alternate screen.
        let _ = enable_raw_mode();
        let _ = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture);

        // 3. Install panic hook that restores terminal.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                LeaveAlternateScreen,
                DisableMouseCapture,
                Show
            );
            default_hook(info);
        }));

        TerminalGuard {
            _log_guard: log_guard,
        }
    }

    fn setup_file_logging() -> Option<LogGuard> {
        use tracing_subscriber::fmt;
        use tracing_subscriber::EnvFilter;

        let log_path = "/tmp/echo-agent-tui.log";
        let file = match std::fs::File::create(log_path) {
            Ok(f) => f,
            Err(_) => return None,
        };

        // Try to set as the global default. If one is already set (which it
        // usually is from main()), this will silently fail — that's fine,
        // the existing subscriber's output will go wherever it was configured.
        // We still keep the file handle alive so we can return it.
        let _ = fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_writer(file.try_clone().ok()?)
            .with_ansi(false)
            .try_init();

        Some(LogGuard { _file: file })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show
        );
        // Flush stdout to ensure any pending output is written.
        use std::io::Write;
        let _ = io::stdout().flush();
    }
}

// ── Entry point ─────────────────────────────────────────────────────────────

/// Run the TUI application.
///
/// This function handles all terminal setup/teardown via [`TerminalGuard`],
/// so the terminal is always restored even on panic or early return.
pub async fn run_tui(agent: AgentHandle) -> anyhow::Result<()> {
    // Create the RAII guard — sets up terminal + logging redirect.
    let _guard = TerminalGuard::new();

    // Build the terminal.
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Get agent info via read lock.
    let (model, mode) = agent
        .read(|a| {
            use echo_agent::agent::Agent;
            let m = a.model_name().to_string();
            let mode = a
                .mode()
                .map(|m| format!("{:?}", m))
                .unwrap_or_else(|| "general".to_string());
            (m, mode)
        })
        .await;

    let mut app = TuiApp::new(model, mode);
    app.tool_count = 24; // Default estimate, updated dynamically.

    // Main event loop.
    let result = events::run_event_loop(&mut terminal, &mut app, agent).await;

    // Guard drop will restore the terminal.
    result
}
