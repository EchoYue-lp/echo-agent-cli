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
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use echo_agent_app_core::context_window::{ContextUsageAccumulator, ContextWindowSnapshot};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::{Terminal, TerminalOptions, Viewport, backend::CrosstermBackend};
use std::collections::VecDeque;
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
    /// Dark theme — Claude Code inspired: warm coral accent on a near-black
    /// canvas, semantic colors for status/roles, muted grays for chrome.
    pub fn dark() -> Self {
        use ratatui::style::Color as RatatuiColor;
        Self {
            is_dark: true,
            bg: RatatuiColor::Rgb(18, 18, 20),
            surface0: RatatuiColor::Rgb(44, 44, 50),
            surface1: RatatuiColor::Rgb(30, 30, 34),
            overlay0: RatatuiColor::Rgb(118, 118, 128),
            text: RatatuiColor::Rgb(232, 230, 228),
            subtext: RatatuiColor::Rgb(156, 156, 166),
            blue: RatatuiColor::Rgb(125, 175, 255),
            green: RatatuiColor::Rgb(88, 203, 134),
            yellow: RatatuiColor::Rgb(229, 192, 123),
            peach: RatatuiColor::Rgb(222, 147, 95),
            mauve: RatatuiColor::Rgb(198, 160, 246),
            teal: RatatuiColor::Rgb(86, 196, 180),
            red: RatatuiColor::Rgb(240, 113, 120),
            cyan: RatatuiColor::Rgb(104, 205, 224),
            lavender: RatatuiColor::Rgb(164, 177, 255),
        }
    }

    /// Light theme — same accent family tuned for a near-white canvas.
    pub fn light() -> Self {
        use ratatui::style::Color as RatatuiColor;
        Self {
            is_dark: false,
            bg: RatatuiColor::Rgb(250, 250, 249),
            surface0: RatatuiColor::Rgb(226, 224, 222),
            surface1: RatatuiColor::Rgb(240, 239, 237),
            overlay0: RatatuiColor::Rgb(148, 148, 158),
            text: RatatuiColor::Rgb(30, 30, 34),
            subtext: RatatuiColor::Rgb(96, 96, 106),
            blue: RatatuiColor::Rgb(28, 99, 218),
            green: RatatuiColor::Rgb(22, 131, 66),
            yellow: RatatuiColor::Rgb(176, 122, 8),
            peach: RatatuiColor::Rgb(187, 87, 36),
            mauve: RatatuiColor::Rgb(128, 66, 200),
            teal: RatatuiColor::Rgb(8, 133, 118),
            red: RatatuiColor::Rgb(198, 40, 44),
            cyan: RatatuiColor::Rgb(0, 128, 158),
            lavender: RatatuiColor::Rgb(88, 98, 218),
        }
    }

    /// Create a Theme from a CLI ColorTheme, unifying both theme systems.
    pub fn from_color_theme(ct: &echo_agent_app_core::output::theme::ColorTheme) -> Self {
        if ct.name == "light" {
            Self::light()
        } else {
            Self::dark()
        }
    }

    /// Apply a plugin theme's semantic colors to the TUI palette.
    pub fn from_plugin_theme(
        definition: &echo_agent_app_core::plugin_runtime::PluginThemeDefinition,
    ) -> Self {
        let mut theme = if definition.dark {
            Self::dark()
        } else {
            Self::light()
        };
        for (key, value) in &definition.colors {
            let Some(color) = parse_plugin_color(value) else {
                continue;
            };
            match key
                .trim_start_matches("--")
                .replace('_', "-")
                .to_ascii_lowercase()
                .as_str()
            {
                "bg" | "background" | "bg-primary" => theme.bg = color,
                "surface0" | "surface-0" | "bg-secondary" => theme.surface0 = color,
                "surface1" | "surface-1" | "bg-tertiary" => theme.surface1 = color,
                "overlay0" | "overlay-0" | "border-primary" | "text-tertiary" => {
                    theme.overlay0 = color;
                }
                "text" | "text-primary" => theme.text = color,
                "subtext" | "text-secondary" => theme.subtext = color,
                "accent" | "peach" => theme.peach = color,
                "blue" => theme.blue = color,
                "green" | "color-success" => theme.green = color,
                "yellow" | "color-warning" => theme.yellow = color,
                "mauve" => theme.mauve = color,
                "teal" => theme.teal = color,
                "red" | "color-error" => theme.red = color,
                "cyan" => theme.cyan = color,
                "lavender" => theme.lavender = color,
                _ => {}
            }
        }
        theme
    }
}

fn parse_plugin_color(value: &str) -> Option<Color> {
    let hex = value.trim().strip_prefix('#')?;
    if hex.chars().count() != 6 || !hex.chars().all(|character| character.is_ascii_hexdigit()) {
        return None;
    }
    let packed = u32::from_str_radix(hex, 16).ok()?;
    let red = u8::try_from((packed >> 16) & 0xff).ok()?;
    let green = u8::try_from((packed >> 8) & 0xff).ok()?;
    let blue = u8::try_from(packed & 0xff).ok()?;
    Some(Color::Rgb(red, green, blue))
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

/// Exact TaskRun identity retained while a TUI resume turn is queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedRunResume {
    pub identity: echo_agent_app_core::tasks::task_runtime::TaskRunResumeIdentity,
}

/// A user turn submitted while the foreground agent is still busy.
#[derive(Clone, Debug)]
pub struct QueuedTurn {
    pub text: String,
    pub attachments: Vec<echo_agent_app_core::attachments::AttachmentRef>,
    pub interaction_mode: InteractionMode,
    pub run_resume: Option<QueuedRunResume>,
}

/// Read-only TUI projection of the authoritative TaskRuntime state.
#[derive(Clone, Debug, Default)]
pub struct TaskRuntimeView {
    pub workspace_id: String,
    pub conversation_id: String,
    pub run_id: String,
    pub run_created_at: chrono::DateTime<chrono::Utc>,
    pub goal: String,
    pub goal_revision: u64,
    pub status: String,
    pub continuation_enabled: bool,
    pub turn_ordinal: Option<u64>,
    pub tokens_used: u64,
    pub token_budget: Option<u64>,
    pub time_used_seconds: u64,
    pub time_budget_seconds: Option<u64>,
    pub compaction_count: u32,
    pub pause_reason: Option<String>,
    pub pause_detail: Option<String>,
    pub deferred: bool,
    pub active_cell_count: usize,
    pub tasks: Vec<TaskRuntimeTaskView>,
    pub completion_ready: bool,
    pub requirements: Vec<TaskRuntimeRequirementView>,
}

#[derive(Clone, Debug)]
pub struct TaskRuntimeTaskView {
    pub title: String,
    pub status: String,
    pub agent_role: String,
}

#[derive(Clone, Debug)]
pub struct TaskRuntimeRequirementView {
    pub requirement_id: String,
    pub title: String,
    pub status: String,
}

/// Live, in-memory projection of one framework subagent dispatch.
#[derive(Clone, Debug, Default)]
pub struct SubagentRuntimeView {
    pub execution_id: String,
    pub agent: String,
    pub task: String,
    pub status: String,
    pub tool_calls: usize,
    pub tokens_used: Option<u64>,
    pub duration_ms: Option<u64>,
    pub background: bool,
    pub summary: String,
    pub artifacts: Vec<String>,
    pub verification: Vec<String>,
    pub remaining_work: Vec<String>,
    pub files_read: Vec<String>,
    pub files_written: Vec<String>,
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
    /// UI correlation id for the authoritative application foreground turn.
    pub active_turn_id: Option<String>,
    /// Workspace identity captured with the active turn.
    pub active_turn_workspace_id: Option<String>,
    /// Conversation identity captured with the active turn.
    pub active_turn_conversation_id: Option<String>,
    /// Execution root captured with the active turn.
    pub active_turn_execution_root: Option<std::path::PathBuf>,
    /// Exact workspace agent retained for steering the active turn.
    pub active_turn_agent: Option<AgentHandle>,
    /// FIFO turns submitted while the foreground agent is busy.
    pub queued_turns: VecDeque<QueuedTurn>,
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
    /// Token usage 累计 (prompt, completion, request_count)。
    /// 注意：prompt/completion 是累计历史值；request_count 用于统计调用次数。
    /// "当前上下文占用"由 context_snapshot 单独维持，见下。
    pub tokens: (u32, u32, u32),
    /// 模型上下文窗口上限（启动时从 agent token_limit 读一次；0 表示未知）。
    pub context_window_size: u32,
    /// 当前上下文窗口占用快照（每次 LlmUsage 后覆盖；压缩后 clear_usage）。
    pub context_snapshot: ContextWindowSnapshot,
    /// 会话级 LLM 用量累计（缓存命中率）；压缩不清，/clear 清零。
    pub usage_accumulator: ContextUsageAccumulator,
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
    /// Built-in theme restored when a plugin theme is deactivated.
    default_theme: Theme,
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
    pub pending_approval: Option<echo_agent_app_core::hitl::PendingApprovalQueue>,
    /// Session-owned HITL transport replayed onto continuation pool agents.
    pub human_loop_provider:
        Option<std::sync::Arc<echo_agent_app_core::hitl::TuiHumanLoopProvider>>,
    /// Shared webhook emitter for chat/tool lifecycle events.
    pub webhook_emitter: Option<std::sync::Arc<echo_agent_app_core::webhook::WebhookEmitter>>,
    /// Shared scheduler used by direct `/cron` commands.
    pub scheduler: Option<std::sync::Arc<echo_agent_app_core::scheduler::SchedulerRunner>>,
    /// Shared live plugin runtime used by direct `/plugins` commands.
    pub plugin_runtime:
        Option<std::sync::Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>>,
    /// Latest TaskRuntime projection for the current conversation.
    pub task_runtime_view: Option<TaskRuntimeView>,
    /// Live subagent dispatches observed from the framework event bus.
    pub subagent_runs: Vec<SubagentRuntimeView>,
    /// Staged attachments from `/attach <path>` (B5.3). The next Enter sends
    /// them alongside the typed text as a multimodal message via
    /// `drive_chat(multimodal=Some)`, then drains the buffer. Empty = plain
    /// text turn.
    pub pending_attachments: Vec<echo_agent_app_core::attachments::AttachmentRef>,
    /// Conversation id for this TUI session (TUI/GUI parity). Binds chat turns
    /// and TaskRuntime runs to one conversation; enables transcript projection.
    /// Generated once per session in `run_tui`.
    pub conversation_id: Option<String>,
    /// File-backed conversation projection shared with GUI and headless entry.
    pub conversation_store: Option<std::sync::Arc<dyn echo_agent::memory::ConversationStore>>,
    /// Shared application authority for workspace transitions and scoped stores.
    pub app_state: Option<std::sync::Arc<echo_agent_app_core::state::AppState>>,
    /// PTY currently attached to the TUI terminal pane.
    pub active_terminal_id: Option<String>,
    /// Bounded raw PTY output rendered by the terminal pane.
    pub terminal_output: Vec<u8>,
    /// Active workspace root used by attachments, long-input artifacts and file views.
    pub workspace_root: Option<std::path::PathBuf>,
    /// Immutable execution scope captured for newly dispatched TUI turns.
    pub workspace_execution_scope: echo_agent_app_core::workspace::WorkspaceExecutionScope,
    /// Runtime-ready configured models exposed by the product configuration.
    pub configured_models: Vec<echo_agent_app_core::model_config::ModelRuntimeConfig>,
    /// Static prompt-module report captured during runtime bootstrap.
    pub prompt_assembly: Option<echo_agent_app_core::project::prompt::PromptAssembly>,
    /// Preserve native terminal scrollback instead of entering the alternate screen.
    pub inline_mode: bool,
    /// Event-loop request to temporarily suspend the TUI and open `$VISUAL`/`$EDITOR`.
    pub external_editor_requested: bool,
    /// Project file requested by `/edit`, opened after the current input event settles.
    pub external_file_editor_requested: Option<std::path::PathBuf>,
    /// Shared browser runtime used by direct TUI browser commands.
    pub browser_runtime: Option<std::sync::Arc<echo_agent_app_core::browser::BrowserRuntime>>,
    /// Project-relative paths used by `@` completion.
    pub project_files: Vec<String>,
    /// Current offset for repeated Ctrl+R reverse-history search.
    pub reverse_search_idx: Option<usize>,
    /// Stable query retained while repeated Ctrl+R walks older matches.
    pub reverse_search_query: Option<String>,
    /// Last idle Esc press, used for the double-Esc rewind gesture.
    pub last_escape_at: Option<Instant>,
    /// Event-loop request to rewind the most recent persisted turn.
    pub rewind_requested: bool,
}

impl TuiApp {
    pub(crate) fn discard_unsubmitted_attachments(&mut self) -> Result<(), String> {
        let mut attachments = std::mem::take(&mut self.pending_attachments);
        attachments.extend(
            self.queued_turns
                .drain(..)
                .flat_map(|turn| turn.attachments),
        );
        echo_agent_app_core::attachments::discard_staged_attachment_refs(&attachments)
    }
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolExecutionStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolExecutionMessage {
    pub call_id: String,
    pub name: String,
    pub args: String,
    pub status: ToolExecutionStatus,
    pub stdout: String,
    pub stderr: String,
    pub log: String,
    pub progress: Option<String>,
    pub truncated: bool,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
    pub metadata: std::collections::HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    /// Tool result with diff display (transactional file patch)
    ToolResult {
        tool_name: String,
    },
    ToolExecution(Box<ToolExecutionMessage>),
}

pub(crate) fn tool_command(name: &str, args: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(args) else {
        return format!("{name} {args}");
    };
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
    };
    match name {
        "shell" => text(&["command"]).unwrap_or("shell").to_string(),
        "read_file" => text(&["path", "file_path"]).unwrap_or("file").to_string(),
        "apply_patch" => "Apply patch".to_string(),
        "view_image" => format!("View {}", text(&["path"]).unwrap_or("image")),
        "grep" | "code_search" | "search_text" => format!(
            "Search \"{}\"",
            text(&["query", "pattern", "symbol"]).unwrap_or("query")
        ),
        "glob" => format!("Find \"{}\"", text(&["pattern"]).unwrap_or("pattern")),
        name if name.starts_with("browser_") => {
            let action = name.trim_start_matches("browser_").replace('_', " ");
            if name == "browser_navigate" {
                format!("Open {}", text(&["url"]).unwrap_or("page"))
            } else {
                format!(
                    "{}{}",
                    action.chars().next().unwrap_or_default().to_uppercase(),
                    action.chars().skip(1).collect::<String>()
                )
            }
        }
        "agent_tool" => format!("Subagent {}", text(&["agent_name"]).unwrap_or("dispatch")),
        "create_complex_task" => "Start task run".to_string(),
        "task_execute" => value
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .map(|revision| format!("Execute task graph r{revision}"))
            .unwrap_or_else(|| "Execute task graph".to_string()),
        name if name.starts_with("mcp__") => {
            let mut parts = name.splitn(3, "__");
            let _prefix = parts.next();
            match (parts.next(), parts.next()) {
                (Some(server), Some(tool)) => format!("{server} · {tool}"),
                _ => name.to_string(),
            }
        }
        _ => format!("{name} {args}"),
    }
}

pub(crate) fn tool_detail(tool: &ToolExecutionMessage) -> String {
    if tool.metadata.get("tool_source").map(String::as_str) == Some("mcp") {
        let identity = [
            tool.metadata.get("mcp_server").cloned(),
            tool.metadata.get("mcp_tool").cloned(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let result_type = tool
            .metadata
            .get("result_type")
            .map(|value| format!("{value} result"));
        return [(!identity.is_empty()).then_some(identity), result_type]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&tool.args) else {
        return String::new();
    };
    match tool.name.as_str() {
        "read_file" => {
            let offset = value
                .get("offset")
                .or_else(|| value.get("start_line"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1);
            let limit = value
                .get("limit")
                .or_else(|| value.get("line_count"))
                .and_then(serde_json::Value::as_i64);
            match limit {
                Some(limit) if limit >= 0 => {
                    let end = offset
                        .saturating_add(limit as u64)
                        .saturating_sub(1)
                        .max(offset);
                    format!("lines {offset}-{end}")
                }
                Some(_) => format!("preview from line {offset}"),
                None => format!("from line {offset}"),
            }
        }
        "grep" | "glob" | "code_search" | "search_text" => {
            let scope = value
                .get("path")
                .and_then(serde_json::Value::as_str)
                .filter(|path| *path != ".")
                .map(|path| format!("in {path}"));
            let count = tool_result_count(&tool.stdout);
            [scope, count]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ")
        }
        "apply_patch" => {
            if value.get("dry_run").and_then(serde_json::Value::as_bool) == Some(true) {
                "dry run".to_string()
            } else {
                String::new()
            }
        }
        name if name.starts_with("browser_") => [
            tool.metadata.get("browser_title").cloned(),
            tool.metadata.get("browser_url").cloned().or_else(|| {
                value
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            }),
            value
                .get("target")
                .or_else(|| value.get("element"))
                .or_else(|| value.get("selector"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · "),
        "agent_tool" => value
            .get("task")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        "create_complex_task" => value
            .get("user_goal")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        "task_execute" => value
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .map(|revision| format!("Committed revision {revision}"))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn tool_result_count(output: &str) -> Option<String> {
    output.lines().rev().find_map(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        words.iter().enumerate().find_map(|(index, word)| {
            let label = word.trim_matches(|character: char| !character.is_alphabetic());
            if !matches!(
                label.to_ascii_lowercase().as_str(),
                "matches" | "results" | "files"
            ) {
                return None;
            }
            words
                .get(index.saturating_sub(1))
                .and_then(|value| {
                    value
                        .trim_matches(|character: char| !character.is_numeric())
                        .parse::<u64>()
                        .ok()
                })
                .map(|count| format!("{count} {label}"))
        })
    })
}

pub(crate) fn tool_shows_success_tail(tool: &ToolExecutionMessage) -> bool {
    !matches!(
        tool.name.as_str(),
        "read_file"
            | "apply_patch"
            | "view_image"
            | "grep"
            | "glob"
            | "code_search"
            | "search_text"
            | "agent_tool"
            | "task_execute"
            | "create_complex_task"
            | "browser_backend"
            | "browser_navigate"
            | "browser_snapshot"
            | "browser_click"
            | "browser_fill"
            | "browser_screenshot"
            | "browser_back"
            | "browser_reload"
            | "browser_tabs"
            | "browser_click_at"
            | "browser_type_at"
            | "browser_scroll"
            | "browser_console"
            | "browser_network"
            | "browser_dom_inspect"
            | "browser_performance_trace"
            | "browser_developer_mode"
    )
}

pub(crate) fn tool_output_tail(tool: &ToolExecutionMessage, max_lines: usize) -> Vec<String> {
    let source = if tool.status == ToolExecutionStatus::Succeeded && !tool_shows_success_tail(tool)
    {
        None
    } else if tool.status == ToolExecutionStatus::Failed && !tool.stderr.is_empty() {
        Some(tool.stderr.as_str())
    } else if !tool.stdout.is_empty() {
        Some(tool.stdout.as_str())
    } else if !tool.stderr.is_empty() {
        Some(tool.stderr.as_str())
    } else if !tool.log.is_empty() {
        Some(tool.log.as_str())
    } else {
        None
    };
    let mut output: Vec<String> = match source {
        Some(source) => {
            let lines: Vec<&str> = source.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            lines
                .get(start..)
                .unwrap_or_default()
                .iter()
                .map(|line| (*line).to_string())
                .collect()
        }
        None => tool.progress.clone().into_iter().collect(),
    };
    if let Some(path) = tool.metadata.get("artifact_path") {
        let status = if std::path::Path::new(path).is_file() {
            "full output"
        } else {
            "full output artifact missing"
        };
        output.push(format!("{status}: {path}"));
    }
    output
}

pub(crate) fn tool_metadata_label(tool: &ToolExecutionMessage) -> String {
    let duration = tool
        .metadata
        .get("duration_ms")
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| format!("{:.1}s", value as f64 / 1000.0));
    let exit_code = tool
        .metadata
        .get("exit_code")
        .map(|value| format!("exit {value}"));
    let truncated = tool.truncated.then(|| "truncated".to_string());
    let failure = tool.metadata.get("failure_category").cloned();
    let artifact = tool.metadata.get("artifact_path").map(|path| {
        if std::path::Path::new(path).is_file() {
            tool.metadata
                .get("artifact_bytes")
                .and_then(|value| value.parse::<u64>().ok())
                .map(|bytes| format!("artifact {:.1} MiB", bytes as f64 / 1_048_576.0))
                .unwrap_or_else(|| "artifact".to_string())
        } else {
            "artifact missing".to_string()
        }
    });
    [duration, exit_code, failure, truncated, artifact]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
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
        let default_theme = theme.clone();
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
            active_turn_id: None,
            active_turn_workspace_id: None,
            active_turn_conversation_id: None,
            active_turn_execution_root: None,
            active_turn_agent: None,
            queued_turns: VecDeque::new(),
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
            context_window_size: 0,
            context_snapshot: ContextWindowSnapshot::default(),
            usage_accumulator: ContextUsageAccumulator::default(),
            tool_count: 0,
            iteration_count: 0,
            active_task: None,
            status_msg: "Ready".to_string(),
            should_quit: false,
            history: vec![],
            history_idx: None,
            permission_mode: "ask".to_string(),
            theme,
            default_theme,
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
            human_loop_provider: None,
            webhook_emitter: None,
            scheduler: None,
            plugin_runtime: None,
            task_runtime_view: None,
            subagent_runs: Vec::new(),
            pending_attachments: Vec::new(),
            conversation_id: None,
            conversation_store: None,
            app_state: None,
            active_terminal_id: None,
            terminal_output: Vec::new(),
            workspace_root: None,
            workspace_execution_scope:
                echo_agent_app_core::workspace::WorkspaceExecutionScope::global("."),
            configured_models: Vec::new(),
            prompt_assembly: None,
            inline_mode: false,
            external_editor_requested: false,
            external_file_editor_requested: None,
            browser_runtime: None,
            project_files: Vec::new(),
            reverse_search_idx: None,
            reverse_search_query: None,
            last_escape_at: None,
            rewind_requested: false,
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
            let Some(msg) = self.messages.get(i) else {
                break;
            };

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
                MessageRole::Assistant
                | MessageRole::ToolResult { .. }
                | MessageRole::ToolExecution(_) => {
                    // Group consecutive Assistant and ToolResult messages
                    let start = i;
                    let mut thinking_count = 0;
                    let mut tool_call_count = 0;
                    let mut has_final_answer = false;

                    while i < self.messages.len() {
                        let Some(current) = self.messages.get(i) else {
                            break;
                        };
                        match &current.role {
                            MessageRole::Assistant => {
                                // Check if this is a thinking message or final answer
                                let content = &current.content;
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
                            MessageRole::ToolExecution(_) => {
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

    /// Take the current input into history without starting an agent turn.
    pub fn take_input(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }

        self.history.push(text.clone());
        self.history_idx = None;

        self.input.clear();
        self.cursor = 0;
        self.suggestions.clear();
        self.chat_scroll = 0;

        Some(text)
    }

    /// Mark a submitted turn as the active foreground turn.
    pub fn start_turn(&mut self, text: &str) {
        self.messages.push(ChatMessage {
            role: MessageRole::User,
            content: text.to_string(),
        });
        self.trim_old_messages();
        self.rebuild_message_groups();
        self.is_processing = true;
        self.streaming_text.clear();
        self.pending_stream.clear();
        self.status_msg = "Thinking...".to_string();
        self.clear_selection();
    }

    /// Submit and immediately start the current input.
    pub fn submit_input(&mut self) -> Option<String> {
        let text = self.take_input()?;
        self.start_turn(&text);
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

    /// Finalize streamed content without projecting a lifecycle terminal.
    ///
    /// Only the exact `TurnSettled` reducer may release the foreground UI slot.
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
        self.trim_old_messages();
        self.rebuild_message_groups();
    }

    /// Check whether the messages cache needs rebuilding.
    pub(crate) fn is_messages_cache_stale(&self) -> bool {
        self.messages.len() != self.chat_cache_msg_count
    }

    pub(crate) fn invalidate_messages_cache(&mut self) {
        self.chat_cache_msg_count = usize::MAX;
    }

    pub(crate) fn has_running_tools(&self) -> bool {
        self.messages.iter().any(|message| {
            matches!(
                &message.role,
                MessageRole::ToolExecution(tool)
                    if tool.status == ToolExecutionStatus::Running
            )
        })
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
                        " ✻ Agent ".to_string(),
                        ratatui::style::Style::default()
                            .fg(theme.peach)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    ratatui::text::Span::styled(
                        " streaming...",
                        ratatui::style::Style::default()
                            .fg(theme.yellow)
                            .add_modifier(ratatui::style::Modifier::ITALIC),
                    ),
                ]));
                let md_lines = markdown::render_markdown(streaming_text, &theme);
                for line in md_lines {
                    lines.push(widgets::chat::indent_line(line, theme.surface0));
                }
            } else if is_processing {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    ratatui::text::Span::styled(
                        " ✻ Agent ".to_string(),
                        ratatui::style::Style::default()
                            .fg(theme.peach)
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
            let removed_chars = self
                .messages
                .get(idx)
                .map(|message| message.content.len())
                .unwrap_or(0);
            running_total = running_total.saturating_sub(removed_chars);
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
        if let Some(entry) = self.history.get(idx) {
            self.input = entry.clone();
        }
        self.cursor = self.input.len();
    }

    /// Navigate history down.
    pub fn history_down(&mut self) {
        match self.history_idx {
            Some(i) if i < self.history.len() - 1 => {
                self.history_idx = Some(i + 1);
                if let Some(entry) = self.history.get(i.saturating_add(1)) {
                    self.input = entry.clone();
                }
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
    pub fn input_height(&self, width: u16) -> u16 {
        let content_width = width.saturating_sub(2).max(1) as usize;
        let visual_rows = self.input.split('\n').fold(0usize, |rows, line| {
            let width = visual_width(line);
            rows.saturating_add(width.max(1).div_ceil(content_width))
        });
        visual_rows.clamp(1, 8) as u16 + 2
    }

    pub fn compute_chat_rect(
        size: Rect,
        sidebar_visible: bool,
        input_height: u16,
        task_strip_rows: u16,
    ) -> Rect {
        let constraints = vec![
            Constraint::Length(1), // StatusBar
            Constraint::Min(8),    // Body
            Constraint::Length(input_height),
            Constraint::Length(task_strip_rows),
        ];
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(size);

        let body = main_chunks.get(1).copied().unwrap_or(size);
        if sidebar_visible {
            let body_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(24), Constraint::Min(40)])
                .split(body);
            body_chunks.get(1).copied().unwrap_or(body)
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
                    text: " ❯ You ".to_string(),
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
                    text: " ✻ Agent ".to_string(),
                    message_idx: msg_idx,
                });
                // Markdown content with indent guide
                let md_lines = markdown::render_markdown(content, &self.theme);
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
                let header = format!(" ▸ {tool_name} {summary}");
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
            MessageRole::ToolExecution(tool) => {
                let elapsed = tool
                    .finished_at
                    .unwrap_or_else(Instant::now)
                    .saturating_duration_since(tool.started_at)
                    .as_secs_f32();
                let symbol = match tool.status {
                    ToolExecutionStatus::Running => "●",
                    ToolExecutionStatus::Succeeded => "✓",
                    ToolExecutionStatus::Failed => "✗",
                    ToolExecutionStatus::Cancelled => "■",
                };
                out.push(WrappedLine {
                    text: String::new(),
                    message_idx: msg_idx,
                });
                let metadata = tool_metadata_label(tool);
                let timing = if metadata.is_empty() {
                    format!("{elapsed:.1}s")
                } else {
                    metadata
                };
                let title = tool_command(&tool.name, &tool.args);
                let detail = tool_detail(tool);
                let header = if detail.is_empty() {
                    format!(" {symbol} {title} · {timing}")
                } else {
                    format!(" {symbol} {title} · {detail} · {timing}")
                };
                for wrapped in textwrap::wrap(&header, wrap_opts) {
                    out.push(WrappedLine {
                        text: wrapped.into_owned(),
                        message_idx: msg_idx,
                    });
                }
                for (i, raw_line) in tool_output_tail(tool, 6).into_iter().enumerate() {
                    let prefix = if i == 0 { "  ⎿ " } else { "    " };
                    let prefixed = format!("{prefix}{raw_line}");
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

            if char_start <= char_end
                && let Some(selected) = text.get(char_start..char_end)
            {
                result.push_str(selected);
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

pub(crate) fn collect_project_files(root: &std::path::Path, limit: usize) -> Vec<String> {
    fn visit(root: &std::path::Path, dir: &std::path::Path, limit: usize, out: &mut Vec<String>) {
        if out.len() >= limit {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            if out.len() >= limit {
                return;
            }
            let path = entry.path();
            let name = entry.file_name();
            let hidden_or_build = name.to_str().is_some_and(|value| {
                matches!(value, ".git" | ".worktrees" | "target" | "node_modules")
            });
            if hidden_or_build {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                visit(root, &path, limit, out);
            } else if file_type.is_file()
                && let Ok(relative) = path.strip_prefix(root)
            {
                out.push(relative.to_string_lossy().to_string());
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, limit, &mut files);
    files.sort();
    files
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod state_tests {
    use super::{Theme, TuiApp, Viewport, tui_viewport};
    use ratatui::style::Color;

    fn app() -> TuiApp {
        let theme =
            Theme::from_color_theme(&echo_agent_app_core::output::theme::ColorTheme::dark());
        TuiApp::new("test-model".to_string(), "test".to_string(), theme)
    }

    #[test]
    fn plugin_theme_maps_css_and_semantic_colors_without_panicking() {
        let definition = echo_agent_app_core::plugin_runtime::PluginThemeDefinition {
            name: "local-theme".to_string(),
            display_name: None,
            dark: false,
            colors: std::collections::HashMap::from([
                ("--bg-primary".to_string(), "#010203".to_string()),
                ("accent".to_string(), "#a0b0c0".to_string()),
                ("red".to_string(), "not-a-color".to_string()),
            ]),
            plugin: "test".to_string(),
        };

        let theme = Theme::from_plugin_theme(&definition);

        assert!(!theme.is_dark);
        assert_eq!(theme.bg, Color::Rgb(1, 2, 3));
        assert_eq!(theme.peach, Color::Rgb(160, 176, 192));
        assert_eq!(theme.red, Theme::light().red);
    }

    #[test]
    fn taking_input_does_not_start_turn_until_dispatched() {
        let mut app = app();
        app.input = "queued request".to_string();
        app.cursor = app.input.len();

        let text = app.take_input();

        assert_eq!(text.as_deref(), Some("queued request"));
        assert!(!app.is_processing);
        assert!(!app.messages.iter().any(|m| m.content == "queued request"));

        app.start_turn("queued request");
        assert!(app.is_processing);
        assert!(app.messages.iter().any(|m| m.content == "queued request"));
    }

    #[test]
    fn input_height_grows_for_multiline_and_caps_at_eight_rows() {
        let mut app = app();
        assert_eq!(app.input_height(80), 3);

        app.input = "一\n二\n三".to_string();
        assert_eq!(app.input_height(80), 5);

        app.input = (0..20).map(|_| "line").collect::<Vec<_>>().join("\n");
        assert_eq!(app.input_height(80), 10);
    }

    #[test]
    fn inline_mode_uses_scrollback_preserving_viewport() {
        assert_eq!(tui_viewport(true, 40), Viewport::Inline(39));
        assert_eq!(tui_viewport(true, 5), Viewport::Inline(10));
        assert_eq!(tui_viewport(false, 40), Viewport::Fullscreen);
    }

    #[test]
    fn finalize_stream_only_commits_content() {
        let mut app = app();
        app.is_processing = true;
        app.active_turn_id = Some("turn-1".to_string());
        app.streaming_text = "done".to_string();

        app.finalize_stream();

        assert!(app.is_processing);
        assert_eq!(app.active_turn_id.as_deref(), Some("turn-1"));
        assert_eq!(app.last_assistant_response(), Some("done"));
    }

    #[tokio::test]
    async fn tui_shutdown_waits_for_foreground_settlement() -> Result<(), String> {
        use echo_agent_app_core::chat_driver::TurnOutcome;
        use echo_agent_app_core::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

        let control = ForegroundTurnControl::default();
        let lease = control
            .begin(ForegroundTurnSurface::Tui, "conversation", "turn-1")
            .map_err(|error| error.to_string())?;
        let cancelled = lease.cancellation_token();
        let driver = tokio::spawn(async move {
            cancelled.cancelled().await;
            tokio::task::yield_now().await;
            let _ = lease.settle(TurnOutcome::Cancelled);
        });

        super::settle_tui_foreground_on_exit(&control, "global", "conversation")
            .await
            .map_err(|error| error.to_string())?;
        driver.await.map_err(|error| error.to_string())?;
        assert!(
            control
                .snapshot(ForegroundTurnSurface::Tui, "conversation")
                .is_none()
        );
        Ok(())
    }
}

// ── RAII Terminal Guard ─────────────────────────────────────────────────────

/// RAII guard that sets up the terminal on creation and restores it on drop.
///
/// Redirects stderr to a log file at the OS file-descriptor level, so the
/// existing tracing subscriber (set up in `main()`) writes to the file
/// instead of corrupting the TUI screen.
struct TerminalGuard {
    inline_mode: bool,
}

fn tui_viewport(inline_mode: bool, terminal_height: u16) -> Viewport {
    if inline_mode {
        Viewport::Inline(terminal_height.saturating_sub(1).max(10))
    } else {
        Viewport::Fullscreen
    }
}

impl TerminalGuard {
    fn new(inline_mode: bool) -> Self {
        // Note: stderr redirect is handled earlier in main.rs by StderrRedirectGuard,
        // so this guard only manages raw mode, alternate screen, and panic hook.

        // 1. Enter raw mode + alternate screen.
        let _ = enable_raw_mode();
        if inline_mode {
            let _ = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste);
        } else {
            let _ = execute!(
                io::stdout(),
                EnterAlternateScreen,
                EnableMouseCapture,
                EnableBracketedPaste
            );
        }

        // 2. Install panic hook that restores terminal.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                DisableBracketedPaste,
                DisableMouseCapture,
                Show
            );
            if !inline_mode {
                let _ = execute!(io::stdout(), LeaveAlternateScreen);
            }
            default_hook(info);
        }));

        TerminalGuard { inline_mode }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            Show
        );
        if !self.inline_mode {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
        use std::io::Write;
        let _ = io::stdout().flush();
    }
}

// ── Entry point ─────────────────────────────────────────────────────────────

async fn settle_tui_foreground_on_exit(
    control: &echo_agent_app_core::foreground_turn::ForegroundTurnControl,
    workspace_id: &str,
    conversation_id: &str,
) -> Result<(), echo_agent_app_core::foreground_turn::ForegroundTurnError> {
    use echo_agent_app_core::foreground_turn::{ForegroundTurnError, ForegroundTurnSurface};

    loop {
        let Some(snapshot) =
            control.snapshot_scoped(workspace_id, ForegroundTurnSurface::Tui, conversation_id)
        else {
            return Ok(());
        };
        match control
            .cancel_and_wait_scoped(
                workspace_id,
                ForegroundTurnSurface::Tui,
                conversation_id,
                &snapshot.active_turn_id,
            )
            .await
        {
            Ok(_) => {}
            Err(
                ForegroundTurnError::NoActiveTurn { .. } | ForegroundTurnError::TurnMismatch { .. },
            ) => {}
            Err(error) => return Err(error),
        }
    }
}

/// Run the TUI application.
///
/// This function handles all terminal setup/teardown via [`TerminalGuard`],
/// so the terminal is always restored even on panic or early return.
#[allow(clippy::too_many_arguments)] // startup entry: agent + shared services + config are wired here
pub async fn run_tui(
    agent: AgentHandle,
    tui_config: &echo_agent_app_core::config::TuiConfig,
    mode_display: &str,
    tui_pending: echo_agent_app_core::hitl::PendingApprovalQueue,
    tui_provider: std::sync::Arc<echo_agent_app_core::hitl::TuiHumanLoopProvider>,
    webhook_emitter: std::sync::Arc<echo_agent_app_core::webhook::WebhookEmitter>,
    scheduler: Option<std::sync::Arc<echo_agent_app_core::scheduler::SchedulerRunner>>,
    conversation_store: Option<std::sync::Arc<dyn echo_agent::memory::ConversationStore>>,
    conversation_id: String,
    configured_models: Vec<echo_agent_app_core::model_config::ModelRuntimeConfig>,
    browser_runtime: std::sync::Arc<echo_agent_app_core::browser::BrowserRuntime>,
    prompt_assembly: echo_agent_app_core::project::prompt::PromptAssembly,
    plugin_runtime: std::sync::Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>,
    app_state: std::sync::Arc<echo_agent_app_core::state::AppState>,
    inline_mode: bool,
) -> anyhow::Result<()> {
    // Use ColorTheme to generate Theme, unifying both theme systems.
    let color_theme = echo_agent_app_core::output::theme::ColorTheme::dark();
    let theme = Theme::from_color_theme(&color_theme);

    // Create the RAII guard — redirects stderr + sets up terminal.
    let _guard = TerminalGuard::new(inline_mode);

    // Build the terminal.
    let backend = CrosstermBackend::new(io::stdout());
    let terminal_height = crossterm::terminal::size()
        .map(|(_, height)| height)
        .unwrap_or(24);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: tui_viewport(inline_mode, terminal_height),
        },
    )?;
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
    if let Some(active) = plugin_runtime.active_theme().await
        && let Some(theme) = plugin_runtime
            .themes()
            .await
            .into_iter()
            .find(|theme| theme.name == active)
    {
        app.theme = Theme::from_plugin_theme(&theme);
    }
    // 读取当前模型的上下文窗口上限（与 GUI panels.rs 同样走 agent.config().get_token_limit()）。
    app.context_window_size = agent.read(|a| a.config().get_token_limit() as u32).await;
    app.context_snapshot.context_window_size = app.context_window_size;
    app.tool_count = agent.read(|value| value.tool_names().len()).await;
    app.permission_mode = agent
        .read(|value| {
            echo_agent_app_core::permission::permission_mode_id(value.get_permission_mode())
                .to_string()
        })
        .await;
    app.max_display_chars = tui_config.max_display_chars;
    app.pending_approval = Some(tui_pending);
    app.human_loop_provider = Some(tui_provider);
    app.webhook_emitter = Some(webhook_emitter);
    app.scheduler = scheduler;
    // One conversation id per TUI session (parity with GUI's per-conversation id):
    // binds this session's chat turns + TaskRuntime runs + transcript projection.
    app.conversation_id = Some(conversation_id.clone());
    app.conversation_store = conversation_store;
    app.app_state = Some(app_state.clone());
    app.workspace_root = app_state
        .current_workspace()
        .await
        .map(|workspace| workspace.root);
    app.workspace_execution_scope = app_state.current_execution_scope().await;
    app.configured_models = configured_models;
    app.prompt_assembly = Some(prompt_assembly);
    app.plugin_runtime = Some(plugin_runtime);
    app.browser_runtime = Some(browser_runtime);
    app.inline_mode = inline_mode;
    let project_root = app
        .workspace_root
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("."));
    app.project_files = collect_project_files(project_root, 10_000);
    if let Some(store) = app.conversation_store.as_ref()
        && let Ok(stored) = store.get_messages(&conversation_id).await
        && !stored.is_empty()
    {
        app.messages = stored
            .into_iter()
            .filter_map(|message| {
                let content = message.content?;
                let role = match message.role.as_str() {
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "tool" => MessageRole::ToolResult {
                        tool_name: "tool".to_string(),
                    },
                    _ => MessageRole::System,
                };
                Some(ChatMessage { role, content })
            })
            .collect();
        app.messages.push(ChatMessage {
            role: MessageRole::System,
            content: format!("Resumed conversation {conversation_id}"),
        });
        app.rebuild_message_groups();
    }
    agent
        .write(|value| value.set_conversation_id(conversation_id.clone()))
        .await;

    // Main event loop.
    let result = events::run_event_loop(&mut terminal, &mut app, agent).await;

    let foreground_workspace_id = app
        .active_turn_workspace_id
        .clone()
        .unwrap_or_else(|| app.workspace_execution_scope.workspace_id().to_string());
    let foreground_shutdown = settle_tui_foreground_on_exit(
        &app_state.session.foreground_turns,
        &foreground_workspace_id,
        &conversation_id,
    )
    .await;
    let attachment_cleanup = app.discard_unsubmitted_attachments();
    // Guard drop will restore the terminal.
    match (result, foreground_shutdown, attachment_cleanup) {
        (Ok(()), Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(()), Ok(())) => Err(error),
        (Ok(()), Err(error), Ok(())) => Err(anyhow::anyhow!(
            "failed to settle TUI foreground turn during shutdown: {error}"
        )),
        (Ok(()), Ok(()), Err(error)) => Err(anyhow::anyhow!(
            "failed to clean TUI attachment staging during shutdown: {error}"
        )),
        (loop_result, shutdown_result, cleanup_result) => Err(anyhow::anyhow!(
            "TUI shutdown did not settle cleanly: event_loop={}; foreground={}; attachments={}",
            loop_result
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "ok".to_string()),
            shutdown_result
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "ok".to_string()),
            cleanup_result.err().unwrap_or_else(|| "ok".to_string())
        )),
    }
}
