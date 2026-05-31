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
use ratatui::style::Color;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;

// ── Theme ───────────────────────────────────────────────────────────────────

/// Color theme that adapts to terminal background.
#[derive(Clone, Debug)]
pub struct Theme {
    /// Whether terminal has a dark background.
    pub is_dark: bool,
    // Base colors
    pub bg: Color,
    pub surface0: Color,
    pub surface1: Color,
    pub overlay0: Color,
    // Text colors
    pub text: Color,
    pub subtext: Color,
    // Accent colors (same for both themes)
    pub blue: Color,
    pub green: Color,
    pub yellow: Color,
    pub peach: Color,
    pub mauve: Color,
    pub teal: Color,
    pub red: Color,
    pub cyan: Color,
    pub lavender: Color,
}

impl Theme {
    /// Dark theme — black background, white text.
    fn dark() -> Self {
        Self {
            is_dark: true,
            bg: Color::Rgb(0, 0, 0),
            surface0: Color::Rgb(20, 20, 20),
            surface1: Color::Rgb(40, 40, 40),
            overlay0: Color::Rgb(100, 100, 100),
            text: Color::Rgb(255, 255, 255),
            subtext: Color::Rgb(180, 180, 180),
            blue: Color::Rgb(100, 149, 237),    // cornflower blue
            green: Color::Rgb(80, 220, 100),
            yellow: Color::Rgb(255, 215, 0),     // gold
            peach: Color::Rgb(255, 165, 80),
            mauve: Color::Rgb(180, 130, 255),
            teal: Color::Rgb(0, 200, 180),
            red: Color::Rgb(255, 80, 80),
            cyan: Color::Rgb(0, 200, 220),
            lavender: Color::Rgb(170, 150, 255),
        }
    }

    /// Catppuccin Latte — for light terminal backgrounds.
    fn light() -> Self {
        Self {
            is_dark: false,
            bg: Color::Rgb(239, 241, 245),
            surface0: Color::Rgb(204, 208, 218),
            surface1: Color::Rgb(188, 192, 204),
            overlay0: Color::Rgb(124, 127, 143),
            text: Color::Rgb(76, 79, 105),
            subtext: Color::Rgb(92, 95, 119),
            blue: Color::Rgb(30, 102, 245),
            green: Color::Rgb(64, 160, 43),
            yellow: Color::Rgb(223, 142, 29),
            peach: Color::Rgb(254, 100, 11),
            mauve: Color::Rgb(136, 57, 239),
            teal: Color::Rgb(23, 146, 153),
            red: Color::Rgb(210, 15, 57),
            cyan: Color::Rgb(4, 165, 229),
            lavender: Color::Rgb(114, 135, 253),
        }
    }
}

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
    pub suggestions: Vec<commands::SlashCommand>,
    /// Selected suggestion index.
    pub selected_suggestion: usize,
    /// Scroll offset for suggestion popup (first visible index).
    pub suggestion_scroll: usize,
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
    /// Color theme (auto-detected from terminal).
    pub theme: Theme,
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
    pub fn new(model: String, mode: String, theme: Theme) -> Self {
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
            suggestion_scroll: 0,
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
            theme,
        }
    }

    /// Update slash command suggestions based on current input.
    pub fn update_suggestions(&mut self) {
        if self.input.starts_with('/') && !self.input.contains(' ') {
            self.suggestions = commands::SlashCommand::complete(&self.input);
            self.selected_suggestion = 0;
            self.suggestion_scroll = 0;
        } else {
            self.suggestions.clear();
            self.suggestion_scroll = 0;
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
/// Redirects stderr to a log file at the OS file-descriptor level, so the
/// existing tracing subscriber (set up in `main()`) writes to the file
/// instead of corrupting the TUI screen.
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Self {
        // Note: stderr redirect is handled earlier in main.rs by StderrRedirectGuard,
        // so this guard only manages raw mode, alternate screen, and panic hook.

        // 1. Enter raw mode + alternate screen.
        let _ = enable_raw_mode();
        let _ = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture);

        // 2. Install panic hook that restores terminal.
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

        TerminalGuard
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
    // Always use dark theme (black bg, white text).
    let theme = Theme::dark();

    // Create the RAII guard — redirects stderr + sets up terminal.
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

    let mut app = TuiApp::new(model, mode, theme);
    app.tool_count = 24; // Default estimate, updated dynamically.

    // Main event loop.
    let result = events::run_event_loop(&mut terminal, &mut app, agent).await;

    // Guard drop will restore the terminal.
    result
}
