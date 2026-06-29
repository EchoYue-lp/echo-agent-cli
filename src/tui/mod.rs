//! Terminal User Interface (TUI) for echo-agent-cli.
//!
//! Full-screen terminal UI built with ratatui, providing:
//! - Status bar: model, mode, token usage, permission
//! - Sidebar: file tree, tools list, active tasks
//! - Chat area: streaming messages with markdown rendering
//! - Input box: slash command completion, multi-line input
//!
//! ## Terminal safety
//!
//! An RAII [`TerminalGuard`] ensures the terminal is always restored on exit,
//! even on panic. Tracing output is redirected to a file so that log messages
//! never corrupt the alternate screen.

pub mod clipboard;
pub mod commands;
pub mod events;
pub mod markdown;
pub mod ui;
pub mod widgets;

use crate::agent_handle::AgentHandle;
use crossterm::{
    cursor::Show,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::time::Instant;
use textwrap::WordSplitter;
use unicode_width::UnicodeWidthChar;

use echo_agent_app_core::tasks::task_runtime::types::InteractionMode;

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
    /// Create a Theme from a CLI ColorTheme, unifying both theme systems.
    pub fn from_color_theme(ct: &echo_agent_app_core::output::theme::ColorTheme) -> Self {
        use ratatui::style::Color as RatatuiColor;

        if ct.name == "light" {
            return Self {
                is_dark: false,
                bg: RatatuiColor::Rgb(250, 250, 249),
                surface0: RatatuiColor::Rgb(231, 229, 228),
                surface1: RatatuiColor::Rgb(245, 245, 244),
                overlay0: RatatuiColor::Rgb(161, 161, 170),
                text: RatatuiColor::Rgb(24, 24, 27),
                subtext: RatatuiColor::Rgb(82, 82, 91),
                blue: RatatuiColor::Rgb(82, 82, 91),
                green: RatatuiColor::Rgb(82, 82, 91),
                yellow: RatatuiColor::Rgb(202, 138, 4),
                peach: RatatuiColor::Rgb(82, 82, 91),
                mauve: RatatuiColor::Rgb(82, 82, 91),
                teal: RatatuiColor::Rgb(82, 82, 91),
                red: RatatuiColor::Rgb(220, 38, 38),
                cyan: RatatuiColor::Rgb(82, 82, 91),
                lavender: RatatuiColor::Rgb(82, 82, 91),
            };
        }

        Self {
            is_dark: true,
            bg: RatatuiColor::Rgb(16, 16, 16),
            surface0: RatatuiColor::Rgb(48, 48, 48),
            surface1: RatatuiColor::Rgb(36, 36, 36),
            overlay0: RatatuiColor::Rgb(113, 113, 122),
            text: RatatuiColor::Rgb(231, 229, 228),
            subtext: RatatuiColor::Rgb(161, 161, 170),
            blue: RatatuiColor::Rgb(161, 161, 170),
            green: RatatuiColor::Rgb(161, 161, 170),
            yellow: RatatuiColor::Rgb(250, 204, 21),
            peach: RatatuiColor::Rgb(161, 161, 170),
            mauve: RatatuiColor::Rgb(161, 161, 170),
            teal: RatatuiColor::Rgb(161, 161, 170),
            red: RatatuiColor::Rgb(248, 113, 113),
            cyan: RatatuiColor::Rgb(161, 161, 170),
            lavender: RatatuiColor::Rgb(161, 161, 170),
        }
    }
}

// ── Public types ────────────────────────────────────────────────────────────

/// Status of a parallel task displayed in the task strip.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskStripStatus {
    /// Waiting to start.
    Pending,
    /// Currently running.
    Running,
    /// Completed successfully.
    Completed,
    /// Failed with an error message.
    Failed(String),
    /// Cancelled.
    Cancelled,
}

/// A single entry in the parallel task progress strip.
#[derive(Clone, Debug)]
pub struct TaskProgressEntry {
    /// Unique task identifier.
    pub task_id: String,
    /// Short display name (e.g. "Research papers", "Generate report").
    pub name: String,
    /// Current status.
    pub status: TaskStripStatus,
    /// Progress percentage (0.0–100.0), used for gauge bar.
    pub progress_pct: f64,
    /// Current phase label (e.g. "Searching", "Analyzing").
    pub phase: String,
    /// Optional detail message (e.g. "12/20 papers found").
    pub message: Option<String>,
    /// When this task was first seen (for elapsed time display).
    pub started_at: Instant,
    /// Elapsed description cache (e.g. "2m 8s").
    pub elapsed_label: String,
}

/// TUI application state.
pub struct TuiApp {
    /// Current input text.
    pub input: String,
    /// Cursor position in input (byte offset).
    pub cursor: usize,
    /// Chat messages (role, content, tool_calls).
    pub messages: Vec<ChatMessage>,
    /// Message groups (for collapsible display).
    pub message_groups: Vec<MessageGroup>,
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
    /// Scroll offset for chat area (0 = auto-scroll to bottom).
    pub chat_scroll: usize,
    /// Model name.
    pub model: String,
    /// Agent mode label.
    pub mode: String,
    /// Manual routing override (Auto/Chat/Task) for the next message. Set by
    /// `/mode`. TUI/GUI parity (AGENTS.md): mirrors the GUI's
    /// `app_state.tasks.interaction_mode` and feeds `drive_chat` the same way.
    pub interaction_mode: InteractionMode,
    /// Token usage (prompt, completion, total).
    pub tokens: (u32, u32, u32),
    /// Tool count.
    pub tool_count: usize,
    /// Current ReAct iteration count (incremented on each ThinkStart).
    pub iteration_count: usize,
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
    /// Color theme (auto-detected from terminal).
    pub theme: Theme,
    /// Parallel task progress entries shown in the task strip.
    pub parallel_tasks: Vec<TaskProgressEntry>,
    /// Clipboard lease — keeps the clipboard handle alive on Linux/X11.
    #[allow(dead_code)]
    pub clipboard_lease: Option<clipboard::ClipboardLease>,
    /// Pre-computed wrapped lines for mouse selection coordinate mapping.
    pub wrapped_lines: Vec<WrappedLine>,
    // ── wrapped_lines cache keys (skip recompute when unchanged) ──
    cached_msg_count: usize,
    cached_wrap_width: u16,
    // ── Chat line cache (skip expensive markdown re-render) ──
    /// Cached rendered lines for finalized messages (stable across streaming updates).
    pub(crate) chat_cached_messages_lines: Vec<Line<'static>>,
    /// Cached rendered lines for the current streaming text (changes frequently).
    pub(crate) chat_cached_stream_lines: Vec<Line<'static>>,
    chat_cache_msg_count: usize,
    chat_cache_stream_len: usize,
    chat_cache_is_processing: bool,
    /// Buffered streaming tokens, flushed to streaming_text each frame.
    pub(crate) pending_stream: String,
    /// Maximum total characters of chat messages to keep in memory (default 20,000).
    /// Oldest messages (excluding the welcome message) are dropped when exceeded.
    pub max_display_chars: usize,
    /// Chat area rectangle on screen (computed each frame before draw).
    pub chat_area: Rect,
    /// Mouse selection start: (wrapped_line_index, visual_column).
    pub selection_start: Option<(usize, usize)>,
    /// Mouse selection end: (wrapped_line_index, visual_column).
    pub selection_end: Option<(usize, usize)>,
    /// Pending approval request from the agent (TUI HITL provider).
    pub pending_approval: Option<
        std::sync::Arc<tokio::sync::Mutex<Option<echo_agent_app_core::hitl::PendingApproval>>>,
    >,
    /// AgentPool for acquiring an isolated agent per background run (Phase B3).
    /// Set by `run_tui` at startup; `handle_enter` reads it to build
    /// `ChatResources` for `drive_chat`. `None` until set.
    pub pool: Option<std::sync::Arc<echo_agent_app_core::agent_pool::AgentPool>>,
    /// TaskRuntimeStore for create_run / cancel_run (Phase B3). Set by `run_tui`.
    pub task_runtime_store:
        Option<std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    /// Staged attachments from `/attach <path>` (B5.3). The next Enter sends
    /// them alongside the typed text as a multimodal message via
    /// `drive_chat(multimodal=Some)`, then drains the buffer. Empty = plain
    /// text turn.
    pub pending_attachments: Vec<echo_agent_app_core::attachments::AttachmentRef>,
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    /// Tool result with diff display (file edit/create/write)
    ToolResult {
        tool_name: String,
    },
}

/// A group of related messages (thinking + tool calls + final answer).
/// Represents one "turn" of the assistant's response.
#[derive(Clone, Debug)]
pub struct MessageGroup {
    /// Index of the first message in this group (in TuiApp::messages).
    pub start_idx: usize,
    /// Index of the last message in this group (exclusive).
    pub end_idx: usize,
    /// Whether this group is collapsed (only show summary).
    pub collapsed: bool,
    /// Group type for display purposes.
    pub group_type: MessageGroupType,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MessageGroupType {
    /// User message (always shown as-is).
    UserMessage,
    /// System message (always shown as-is).
    SystemMessage,
    /// Assistant turn with thinking, tool calls, and final answer.
    AssistantTurn {
        thinking_count: usize,
        tool_call_count: usize,
        has_final_answer: bool,
    },
}

/// A single wrapped line of plain text for mouse selection coordinate mapping.
///
/// The text includes display prefixes (indent, guide chars) so its visual width
/// matches what ratatui's Paragraph widget renders.
#[derive(Clone, Debug)]
pub struct WrappedLine {
    /// Plain text of this line (including any prefix/indent).
    pub text: String,
    /// Index into `TuiApp::messages` this line belongs to.
    pub message_idx: usize,
}

// ── TuiApp methods ──────────────────────────────────────────────────────────

impl TuiApp {
    pub fn new(model: String, mode: String, theme: Theme) -> Self {
        Self {
            input: String::new(),
            cursor: 0,
            messages: vec![ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "EKO · {mode} · {model}\n输入消息开始协作，/ 查看命令，Ctrl+C 退出。"
                ),
            }],
            message_groups: vec![],
            is_processing: false,
            streaming_text: String::new(),
            suggestions: vec![],
            selected_suggestion: 0,
            suggestion_scroll: 0,
            sidebar_visible: false,
            sidebar_tab: 0,
            chat_scroll: 0,
            model,
            mode,
            interaction_mode: InteractionMode::default(),
            tokens: (0, 0, 0),
            tool_count: 0,
            iteration_count: 0,
            active_task: None,
            status_msg: "Ready".to_string(),
            should_quit: false,
            history: vec![],
            history_idx: None,
            permission_mode: "ask".to_string(),
            theme,
            parallel_tasks: vec![],
            clipboard_lease: None,
            wrapped_lines: vec![],
            cached_msg_count: 0,
            cached_wrap_width: 0,
            chat_cached_messages_lines: vec![],
            chat_cached_stream_lines: vec![],
            chat_cache_msg_count: 0,
            chat_cache_stream_len: 0,
            chat_cache_is_processing: false,
            pending_stream: String::new(),
            max_display_chars: 20_000,
            chat_area: Rect::new(0, 0, 0, 0),
            selection_start: None,
            selection_end: None,
            pending_approval: None,
            pool: None,
            task_runtime_store: None,
            pending_attachments: Vec::new(),
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

    /// Rebuild message groups from the messages array.
    /// Groups consecutive Assistant messages (thinking + tool calls + final answer) together.
    pub fn rebuild_message_groups(&mut self) {
        self.message_groups.clear();

        let mut i = 0;
        while i < self.messages.len() {
            let msg = &self.messages[i];

            match msg.role {
                MessageRole::User => {
                    // User messages are their own group
                    self.message_groups.push(MessageGroup {
                        start_idx: i,
                        end_idx: i + 1,
                        collapsed: false,
                        group_type: MessageGroupType::UserMessage,
                    });
                    i += 1;
                }
                MessageRole::System => {
                    // System messages are their own group
                    self.message_groups.push(MessageGroup {
                        start_idx: i,
                        end_idx: i + 1,
                        collapsed: false,
                        group_type: MessageGroupType::SystemMessage,
                    });
                    i += 1;
                }
                MessageRole::Assistant | MessageRole::ToolResult { .. } => {
                    // Group consecutive Assistant and ToolResult messages
                    let start = i;
                    let mut thinking_count = 0;
                    let mut tool_call_count = 0;
                    let mut has_final_answer = false;

                    while i < self.messages.len() {
                        match &self.messages[i].role {
                            MessageRole::Assistant => {
                                // Check if this is a thinking message or final answer
                                let content = &self.messages[i].content;
                                if content.contains("🤔") || content.contains("Thinking:") {
                                    thinking_count += 1;
                                } else if !content.is_empty() {
                                    has_final_answer = true;
                                }
                                i += 1;
                            }
                            MessageRole::ToolResult { .. } => {
                                tool_call_count += 1;
                                i += 1;
                            }
                            _ => break,
                        }
                    }

                    self.message_groups.push(MessageGroup {
                        start_idx: start,
                        end_idx: i,
                        collapsed: true, // Default to collapsed for assistant turns
                        group_type: MessageGroupType::AssistantTurn {
                            thinking_count,
                            tool_call_count,
                            has_final_answer,
                        },
                    });
                }
            }
        }
    }

    /// Rebuild chat cache using message groups for collapsible display.
    fn rebuild_chat_cache_with_groups(&mut self, theme: &Theme) {
        self.chat_cached_messages_lines.clear();

        for group in &self.message_groups {
            match &group.group_type {
                MessageGroupType::UserMessage | MessageGroupType::SystemMessage => {
                    // Render user and system messages normally
                    for idx in group.start_idx..group.end_idx {
                        if let Some(msg) = self.messages.get(idx) {
                            widgets::chat::build_chat_lines(
                                &mut self.chat_cached_messages_lines,
                                &msg.role,
                                &msg.content,
                                theme,
                            );
                        }
                    }
                }
                MessageGroupType::AssistantTurn {
                    thinking_count,
                    tool_call_count,
                    has_final_answer,
                } => {
                    if group.collapsed {
                        // Render collapsed summary
                        let mut summary_parts = vec![];
                        if *thinking_count > 0 {
                            summary_parts.push(format!("{} 思考", thinking_count));
                        }
                        if *tool_call_count > 0 {
                            summary_parts.push(format!("{} 工具", tool_call_count));
                        }
                        if *has_final_answer {
                            summary_parts.push("✅ 最终答案".to_string());
                        }

                        let summary = if summary_parts.is_empty() {
                            "🤖 Assistant Turn [▶ 展开]".to_string()
                        } else {
                            format!("🤖 Assistant Turn: {} [▶ 展开]", summary_parts.join(", "))
                        };

                        self.chat_cached_messages_lines.push(Line::from(""));
                        self.chat_cached_messages_lines
                            .push(Line::from(vec![Span::styled(
                                summary,
                                Style::default().fg(theme.blue).add_modifier(Modifier::BOLD),
                            )]));
                    } else {
                        // Render expanded (all messages in the group)
                        for idx in group.start_idx..group.end_idx {
                            if let Some(msg) = self.messages.get(idx) {
                                widgets::chat::build_chat_lines(
                                    &mut self.chat_cached_messages_lines,
                                    &msg.role,
                                    &msg.content,
                                    theme,
                                );
                            }
                        }

                        // Add collapse hint
                        self.chat_cached_messages_lines
                            .push(Line::from(vec![Span::styled(
                                "  [▼ 折叠]",
                                Style::default()
                                    .fg(theme.overlay0)
                                    .add_modifier(Modifier::ITALIC),
                            )]));
                    }
                }
            }
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
        });
        self.trim_old_messages();
        self.rebuild_message_groups();

        self.input.clear();
        self.cursor = 0;
        self.suggestions.clear();
        self.chat_scroll = 0;

        self.is_processing = true;
        self.streaming_text.clear();
        self.status_msg = "Thinking...".to_string();
        self.clear_selection();

        Some(text)
    }

    /// Append streaming text from agent into the pending buffer.
    ///
    /// The buffer is flushed to `streaming_text` periodically by
    /// [`flush_pending_stream`](Self::flush_pending_stream), which avoids
    /// expensive markdown re-renders on every incoming token.
    pub fn append_stream(&mut self, text: &str) {
        self.pending_stream.push_str(text);
    }

    /// Flush buffered streaming tokens into `streaming_text`.
    ///
    /// Called once per frame in the event loop, before draw.
    /// With real streaming (chat_stream), tokens arrive at a reasonable rate
    /// so we flush every frame for smooth display.
    pub fn flush_pending_stream(&mut self) -> bool {
        if self.pending_stream.is_empty() {
            return false;
        }
        self.streaming_text.push_str(&self.pending_stream);
        self.pending_stream.clear();
        true
    }

    /// Finalize the current streaming response.
    pub fn finalize_stream(&mut self) {
        // Flush any remaining buffered tokens.
        if !self.pending_stream.is_empty() {
            self.streaming_text.push_str(&self.pending_stream);
            self.pending_stream.clear();
        }
        if !self.streaming_text.is_empty() {
            self.messages.push(ChatMessage {
                role: MessageRole::Assistant,
                content: self.streaming_text.clone(),
            });
            self.streaming_text.clear();
        }
        self.is_processing = false;
        self.status_msg = "Ready".to_string();
        self.trim_old_messages();
        self.rebuild_message_groups();
    }

    /// Check whether the messages cache needs rebuilding.
    pub(crate) fn is_messages_cache_stale(&self) -> bool {
        self.messages.len() != self.chat_cache_msg_count
    }

    /// Check whether the stream cache needs rebuilding.
    pub(crate) fn is_stream_cache_stale(&self) -> bool {
        self.streaming_text.len() != self.chat_cache_stream_len
            || self.is_processing != self.chat_cache_is_processing
    }

    /// Update cache keys after rebuild.
    pub(crate) fn update_cache_keys(&mut self) {
        self.chat_cache_msg_count = self.messages.len();
        self.chat_cache_stream_len = self.streaming_text.len();
        self.chat_cache_is_processing = self.is_processing;
    }

    /// Rebuild the chat line cache if stale. Called once per frame before draw.
    /// Messages and streaming text are cached separately to avoid re-rendering
    /// all historical messages on every streaming update.
    pub(crate) fn prepare_chat_cache(&mut self) {
        let msg_stale = self.is_messages_cache_stale();
        let stream_stale = self.is_stream_cache_stale();

        if !msg_stale && !stream_stale {
            return;
        }

        let theme = self.theme.clone();

        // Rebuild messages cache only when messages change.
        // Incremental: only render newly appended messages.
        if msg_stale {
            let cached_count = self.chat_cache_msg_count;
            if self.messages.len() < cached_count {
                // Messages were removed — full rebuild.
                self.chat_cached_messages_lines.clear();
            }

            // Rebuild cache using message groups for collapsible display
            self.rebuild_chat_cache_with_groups(&theme);
        }

        // Rebuild stream cache when streaming text changes.
        if stream_stale {
            let mut lines: Vec<Line<'static>> = Vec::new();
            let is_processing = self.is_processing;
            let streaming_text = &self.streaming_text;

            if is_processing && !streaming_text.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    ratatui::text::Span::styled(
                        format!(" {} Agent ", "\u{2728}"),
                        ratatui::style::Style::default()
                            .fg(theme.green)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    ratatui::text::Span::styled(
                        " streaming...",
                        ratatui::style::Style::default()
                            .fg(theme.yellow)
                            .add_modifier(ratatui::style::Modifier::ITALIC),
                    ),
                ]));
                let md_lines = markdown::render_markdown(streaming_text);
                for line in md_lines {
                    lines.push(widgets::chat::indent_line(line, theme.surface0));
                }
            } else if is_processing {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    ratatui::text::Span::styled(
                        format!(" {} Agent ", "\u{2728}"),
                        ratatui::style::Style::default()
                            .fg(theme.green)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    ratatui::text::Span::styled(
                        format!(" {} thinking...", "\u{25dc}"),
                        ratatui::style::Style::default()
                            .fg(theme.yellow)
                            .add_modifier(ratatui::style::Modifier::ITALIC),
                    ),
                ]));
            }
            self.chat_cached_stream_lines = lines;
        }

        self.update_cache_keys();
    }

    /// Trim oldest messages (keeping the welcome message) when total content
    /// exceeds `max_display_chars`.
    fn trim_old_messages(&mut self) {
        let total: usize = self.messages.iter().map(|m| m.content.len()).sum();
        if total <= self.max_display_chars {
            return;
        }

        // Keep messages[0] (welcome), drop from messages[1..] front-to-back.
        let mut removed = 0;
        let mut running_total = total;
        while running_total > self.max_display_chars && 1 + removed < self.messages.len() {
            let idx = 1 + removed; // skip welcome message at index 0
            running_total -= self.messages[idx].content.len();
            removed += 1;
        }
        if removed > 0 {
            self.messages.drain(1..=removed);
        }
    }

    /// Get the content of the last assistant response, if any.
    pub fn last_assistant_response(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .map(|m| m.content.as_str())
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

    // ── Mouse selection ─────────────────────────────────────────────────

    /// Clear the current mouse selection.
    pub fn clear_selection(&mut self) {
        self.selection_start = None;
        self.selection_end = None;
    }

    /// Compute the chat area rect accounting for sidebar visibility.
    pub fn compute_chat_rect(size: Rect, sidebar_visible: bool) -> Rect {
        let constraints = vec![
            Constraint::Length(1), // StatusBar
            Constraint::Min(8),    // Body
            Constraint::Length(2), // Input
        ];
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(size);

        let body = main_chunks[1];
        if sidebar_visible {
            let body_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(24), Constraint::Min(40)])
                .split(body);
            body_chunks[1] // chat area (right of sidebar)
        } else {
            body // full width
        }
    }

    /// Pre-compute wrapped plain-text lines for mouse selection coordinate mapping.
    ///
    /// Uses the same wrapping algorithm as ratatui's Paragraph widget
    /// (`textwrap` with `WordSplitter::NoHyphenation`), so the line indices
    /// and visual column offsets match the rendered output exactly.
    pub fn update_wrapped_lines(&mut self, width: u16) {
        if width == 0 {
            self.wrapped_lines.clear();
            return;
        }

        // ── Cache guard: skip expensive re-render when nothing changed ──
        let msg_count = self.messages.len();
        if msg_count == self.cached_msg_count && width == self.cached_wrap_width {
            return;
        }
        self.cached_msg_count = msg_count;
        self.cached_wrap_width = width;

        let w = width as usize;
        let wrap_opts = textwrap::Options::new(w).word_splitter(WordSplitter::NoHyphenation);
        let mut result = Vec::new();

        for (msg_idx, msg) in self.messages.iter().enumerate() {
            self.push_message_lines(&mut result, msg_idx, &msg.role, &msg.content, &wrap_opts);
        }

        self.wrapped_lines = result;
    }

    /// Generate wrapped plain-text lines for a single message.
    fn push_message_lines(
        &self,
        out: &mut Vec<WrappedLine>,
        msg_idx: usize,
        role: &MessageRole,
        content: &str,
        wrap_opts: &textwrap::Options<'_>,
    ) {
        match role {
            MessageRole::User => {
                // Empty line before message
                out.push(WrappedLine {
                    text: String::new(),
                    message_idx: msg_idx,
                });
                // Badge
                out.push(WrappedLine {
                    text: " \u{1f464} You ".to_string(),
                    message_idx: msg_idx,
                });
                // Content with 4-space indent
                for line in content.lines() {
                    let prefixed = format!("    {line}");
                    for wrapped in textwrap::wrap(&prefixed, wrap_opts) {
                        out.push(WrappedLine {
                            text: wrapped.into_owned(),
                            message_idx: msg_idx,
                        });
                    }
                }
            }
            MessageRole::Assistant => {
                // Empty line before message
                out.push(WrappedLine {
                    text: String::new(),
                    message_idx: msg_idx,
                });
                // Badge
                out.push(WrappedLine {
                    text: " \u{2728} Agent ".to_string(),
                    message_idx: msg_idx,
                });
                // Markdown content with indent guide
                let md_lines = markdown::render_markdown(content);
                for line in &md_lines {
                    let plain = format!("  \u{2502} {line}");
                    for wrapped in textwrap::wrap(&plain, wrap_opts) {
                        out.push(WrappedLine {
                            text: wrapped.into_owned(),
                            message_idx: msg_idx,
                        });
                    }
                }
            }
            MessageRole::System => {
                out.push(WrappedLine {
                    text: String::new(),
                    message_idx: msg_idx,
                });
                let line = format!(" \u{2139}  {content}");
                for wrapped in textwrap::wrap(&line, wrap_opts) {
                    out.push(WrappedLine {
                        text: wrapped.into_owned(),
                        message_idx: msg_idx,
                    });
                }
            }
            MessageRole::ToolResult { tool_name } => {
                out.push(WrappedLine {
                    text: String::new(),
                    message_idx: msg_idx,
                });
                // Header line
                let summary = content.lines().next().unwrap_or(content);
                let header = format!(" \u{1f4dd} {tool_name} {summary}");
                for wrapped in textwrap::wrap(&header, wrap_opts) {
                    out.push(WrappedLine {
                        text: wrapped.into_owned(),
                        message_idx: msg_idx,
                    });
                }
                // Diff lines
                for raw_line in content.lines().skip(1) {
                    let clean = widgets::chat::strip_ansi(raw_line);
                    let prefixed = format!("  {clean}");
                    for wrapped in textwrap::wrap(&prefixed, wrap_opts) {
                        out.push(WrappedLine {
                            text: wrapped.into_owned(),
                            message_idx: msg_idx,
                        });
                    }
                }
            }
        }
    }

    /// Convert screen coordinates to (wrapped_line_index, visual_column).
    ///
    /// Returns `None` if the coordinates are outside the chat area.
    pub fn screen_to_text(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        let ca = self.chat_area;
        if col < ca.x || col >= ca.x + ca.width || row < ca.y || row >= ca.y + ca.height {
            return None;
        }
        if self.wrapped_lines.is_empty() {
            return None;
        }

        let rel_row = (row - ca.y) as usize;
        let rel_col = (col - ca.x) as usize;

        // Compute scroll offset (same algorithm as Chat widget).
        let total_lines = self.wrapped_lines.len();
        let visible = ca.height as usize;
        let max_scroll = total_lines.saturating_sub(visible);
        let scroll = max_scroll.saturating_sub(self.chat_scroll.min(max_scroll));

        let line_idx = (rel_row + scroll).min(self.wrapped_lines.len().saturating_sub(1));
        Some((line_idx, rel_col))
    }

    /// Like `screen_to_text` but clamps row to chat area bounds (for drag outside).
    ///
    /// Column is clamped to chat area; row is clamped so dragging above/below
    /// the chat area still extends the selection to the first/last visible line.
    pub fn screen_to_text_clamped(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        let ca = self.chat_area;
        if self.wrapped_lines.is_empty() || ca.width == 0 || ca.height == 0 {
            return None;
        }

        // Clamp column to chat area.
        let clamped_col = col.max(ca.x).min(ca.x + ca.width - 1);
        // Clamp row to chat area.
        let clamped_row = row.max(ca.y).min(ca.y + ca.height - 1);

        let rel_row = (clamped_row - ca.y) as usize;
        let rel_col = (clamped_col - ca.x) as usize;

        let total_lines = self.wrapped_lines.len();
        let visible = ca.height as usize;
        let max_scroll = total_lines.saturating_sub(visible);
        let scroll = max_scroll.saturating_sub(self.chat_scroll.min(max_scroll));

        let line_idx = (rel_row + scroll).min(self.wrapped_lines.len().saturating_sub(1));
        Some((line_idx, rel_col))
    }

    /// Extract the text covered by the current selection.
    pub fn extract_selected_text(&self) -> String {
        let (start, end) = match (self.selection_start, self.selection_end) {
            (Some(s), Some(e)) => normalize_selection(s, e),
            _ => return String::new(),
        };

        let mut result = String::new();
        for line_idx in start.0..=end.0 {
            let Some(wl) = self.wrapped_lines.get(line_idx) else {
                continue;
            };
            let text = &wl.text;
            if text.is_empty() {
                if line_idx != start.0 || line_idx != end.0 {
                    result.push('\n');
                }
                continue;
            }

            let start_col = if line_idx == start.0 { start.1 } else { 0 };
            let end_col = if line_idx == end.0 {
                end.1
            } else {
                visual_width(text)
            };

            if start_col >= end_col {
                if line_idx != start.0 || line_idx != end.0 {
                    result.push('\n');
                }
                continue;
            }

            let char_start = visual_col_to_char_idx(text, start_col);
            let char_end = visual_col_to_char_idx(text, end_col).min(text.len());

            if char_start <= char_end {
                result.push_str(&text[char_start..char_end]);
            }
            if line_idx < end.0 {
                result.push('\n');
            }
        }
        result
    }

    /// Get the normalized selection range (start <= end), or None if empty.
    pub fn normalized_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        match (self.selection_start, self.selection_end) {
            (Some(s), Some(e)) => {
                let (start, end) = normalize_selection(s, e);
                // Consider selection non-empty only if there's actual range
                if start.0 != end.0 || start.1 != end.1 {
                    Some((start, end))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

// ── Selection helpers (free functions) ──────────────────────────────────────

/// Normalize selection so that start <= end.
fn normalize_selection(a: (usize, usize), b: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) {
        (a, b)
    } else {
        (b, a)
    }
}

/// Compute the visual display width of a string (terminal columns).
fn visual_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(1))
        .sum()
}

/// Convert a visual column offset to a byte index in the string.
///
/// Handles wide characters (emoji, CJK) that occupy 2 terminal columns.
fn visual_col_to_char_idx(text: &str, visual_col: usize) -> usize {
    let mut vcol = 0usize;
    for (byte_idx, ch) in text.char_indices() {
        if vcol >= visual_col {
            return byte_idx;
        }
        vcol += UnicodeWidthChar::width(ch).unwrap_or(1);
    }
    text.len()
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
pub async fn run_tui(
    agent: AgentHandle,
    task_service: Option<std::sync::Arc<echo_agent_app_core::tasks::BackgroundTaskService>>,
    tui_config: &echo_agent_app_core::config::TuiConfig,
    mode_display: &str,
    tui_pending: std::sync::Arc<
        tokio::sync::Mutex<Option<echo_agent_app_core::hitl::PendingApproval>>,
    >,
    pool: std::sync::Arc<echo_agent_app_core::agent_pool::AgentPool>,
    task_runtime_store: Option<
        std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
    >,
) -> anyhow::Result<()> {
    // Use ColorTheme to generate Theme, unifying both theme systems.
    let color_theme = echo_agent_app_core::output::theme::ColorTheme::dark();
    let theme = Theme::from_color_theme(&color_theme);

    // Create the RAII guard — redirects stderr + sets up terminal.
    let _guard = TerminalGuard::new();

    // Build the terminal.
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Get agent model name via read lock.
    let model = agent
        .read(|a| {
            use echo_agent::agent::Agent;
            a.model_name().to_string()
        })
        .await;
    let mode = mode_display.to_string();

    let mut app = TuiApp::new(model, mode, theme);
    app.tool_count = 24; // Default estimate, updated dynamically.
    app.max_display_chars = tui_config.max_display_chars;
    app.pending_approval = Some(tui_pending);
    app.pool = Some(pool);
    app.task_runtime_store = task_runtime_store;

    // Main event loop.
    let result = events::run_event_loop(&mut terminal, &mut app, agent, task_service).await;

    // Guard drop will restore the terminal.
    result
}
