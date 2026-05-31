//! Terminal User Interface (TUI) for echo-agent-cli.
//!
//! Full-screen terminal UI built with ratatui, providing:
//! - Status bar: model, mode, token usage
//! - Sidebar: file tree, tools list, active tasks
//! - Chat area: streaming messages with markdown rendering
//! - Input box: slash command completion, multi-line input
//! - Diff popup: inline code diff preview
//! - Approval cards: human-in-the-loop tool approval

pub mod events;
pub mod ui;

use crate::agent_handle::AgentHandle;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::sync::Arc;

/// TUI application state.
pub struct TuiApp {
    /// Current input text
    pub input: String,
    /// Cursor position in input
    pub cursor: usize,
    /// Chat messages (role, content, is_streaming)
    pub messages: Vec<ChatMessage>,
    /// Whether the agent is currently processing
    pub is_processing: bool,
    /// Current streaming text being received
    pub streaming_text: String,
    /// Slash command suggestions
    pub suggestions: Vec<String>,
    /// Selected suggestion index
    pub selected_suggestion: usize,
    /// Whether sidebar is visible
    pub sidebar_visible: bool,
    /// Sidebar active tab (0=files, 1=tools, 2=tasks)
    pub sidebar_tab: usize,
    /// Whether diff popup is open
    pub diff_popup: Option<DiffPopup>,
    /// Whether approval card is showing
    pub approval: Option<ApprovalRequest>,
    /// Scroll offset for chat area
    pub chat_scroll: u16,
    /// Model name
    pub model: String,
    /// Agent mode
    pub mode: String,
    /// Token usage (prompt, completion, total)
    pub tokens: (u32, u32, u32),
    /// Tool count
    pub tool_count: usize,
    /// Active task name
    pub active_task: Option<String>,
    /// Status message
    pub status_msg: String,
    /// Whether to quit
    pub should_quit: bool,
    /// Input history
    pub history: Vec<String>,
    /// History index
    pub history_idx: Option<usize>,
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

impl TuiApp {
    pub fn new(model: String, mode: String) -> Self {
        Self {
            input: String::new(),
            cursor: 0,
            messages: vec![ChatMessage {
                role: MessageRole::System,
                content: format!("EchoCoWork TUI — mode: {mode}, model: {model}\nType a message or / for commands. Ctrl+C to quit."),
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
        }
    }

    /// Update slash command suggestions based on current input.
    pub fn update_suggestions(&mut self) {
        if self.input.starts_with('/') && !self.input.contains(' ') {
            let query = self.input.to_lowercase();
            let commands = [
                "/help", "/mode", "/model", "/permission", "/tools", "/tasks",
                "/diff", "/cron", "/auto-memory", "/compress", "/memory",
                "/git", "/plan", "/test", "/reset", "/history", "/stats",
                "/pipeline", "/remember", "/forget", "/compact",
            ];
            self.suggestions = commands
                .iter()
                .filter(|c| c.starts_with(&query))
                .map(|c| c.to_string())
                .collect();
            self.selected_suggestion = 0;
        } else {
            self.suggestions.clear();
        }
    }

    /// Submit the current input.
    pub fn submit_input(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }

        // Add to history
        self.history.push(text.clone());
        self.history_idx = None;

        // Add user message
        self.messages.push(ChatMessage {
            role: MessageRole::User,
            content: text.clone(),
            tool_calls: vec![],
        });

        // Clear input
        self.input.clear();
        self.cursor = 0;
        self.suggestions.clear();
        self.chat_scroll = 0;

        // Mark as processing
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

/// Run the TUI application.
pub async fn run_tui(agent: AgentHandle) -> anyhow::Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Get agent info via read lock
    let (model, mode) = agent
        .read(|a| {
            use echo_agent::agent::Agent;
            let m = a.model_name().to_string();
            let mode = a.mode().map(|m| format!("{:?}", m)).unwrap_or_else(|| "general".to_string());
            (m, mode)
        })
        .await;

    let mut app = TuiApp::new(model, mode);
    app.tool_count = 24; // Default estimate, updated dynamically

    // Main event loop
    let result = events::run_event_loop(&mut terminal, &mut app, agent).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}
