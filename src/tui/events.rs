//! TUI event loop — handles keyboard input, terminal resize, and agent streaming.

use super::{
    ChatMessage, MessageRole, QueuedTurn, SubagentRuntimeView, TaskRuntimeTaskView,
    TaskRuntimeView, ToolExecutionMessage, ToolExecutionStatus, TuiApp,
};
use crate::agent_handle::AgentHandle;
use crate::tui::clipboard;
use crate::tui::commands::SlashCommand;
use crate::tui::ui;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::FutureExt;
use ratatui::layout::Rect;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use echo_agent::agent::subagent::SubagentEvent;
use echo_agent::tools::ToolFailure;
use echo_agent_app_core::chat_driver::TurnOutcome;
use echo_agent_app_core::context_window::ContextWindowSnapshot;
use echo_agent_app_core::foreground_turn::{
    ForegroundTurnLease, ForegroundTurnSnapshot, ForegroundTurnSurface,
};

/// Poll interval for non-blocking event check.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const PASTE_ATTACHMENT_CHAR_THRESHOLD: usize = 1_000;

/// Handle keyboard input when an approval request is pending.
/// Returns `true` if the key was consumed.
async fn handle_approval_key(
    _app: &mut TuiApp,
    pending_handle: &echo_agent_app_core::hitl::PendingApprovalQueue,
    key: &KeyEvent,
) -> bool {
    use echo_agent::human_loop::HumanLoopResponse;
    use echo_agent_app_core::hitl::PendingHumanLoopKind;

    let mut guard = match pending_handle.try_lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    echo_agent_app_core::hitl::prune_closed_pending(&mut guard);
    let approval = match guard.front_mut() {
        Some(a) => a,
        None => return false,
    };

    if approval.input_mode {
        // ── Feedback input mode (for 拒绝/修改) ──
        match key.code {
            KeyCode::Esc => {
                if approval.kind == PendingHumanLoopKind::Input {
                    let request_id = approval.request_id.clone();
                    send_and_remove_front(
                        &mut guard,
                        &request_id,
                        HumanLoopResponse::Rejected {
                            reason: Some("User dismissed".to_string()),
                        },
                    );
                } else {
                    approval.input_mode = false;
                    approval.feedback_input.clear();
                    approval.feedback_cursor = 0;
                }
                true
            }
            KeyCode::Enter => {
                let feedback = approval.feedback_input.clone();
                let response = if approval.kind == PendingHumanLoopKind::Input {
                    HumanLoopResponse::Text(feedback)
                } else {
                    let label = approval.input_label.clone();
                    let reason = if feedback.is_empty() {
                        format!("用户{label}")
                    } else {
                        format!("用户{label}: {feedback}")
                    };
                    HumanLoopResponse::Rejected {
                        reason: Some(reason),
                    }
                };
                let request_id = approval.request_id.clone();
                send_and_remove_front(&mut guard, &request_id, response);
                true
            }
            KeyCode::Backspace => {
                if approval.feedback_cursor > 0 {
                    // Remove character before cursor
                    let s = &mut approval.feedback_input;
                    let byte_idx = approval.feedback_cursor;
                    // Find the start of the previous character
                    let prev = s
                        .get(..byte_idx)
                        .unwrap_or_default()
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
                    approval.feedback_cursor = s
                        .get(..approval.feedback_cursor)
                        .unwrap_or_default()
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
                    approval.feedback_cursor += s
                        .get(approval.feedback_cursor..)
                        .unwrap_or_default()
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
                } else if approval.kind == PendingHumanLoopKind::Selection
                    && approval.option_count() > 0
                {
                    approval.selected_option = approval.option_count().saturating_sub(1);
                }
                true
            }
            KeyCode::Right | KeyCode::Tab => {
                let option_count = approval.option_count();
                if option_count > 0 {
                    approval.selected_option =
                        approval.selected_option.saturating_add(1) % option_count;
                }
                true
            }
            KeyCode::Enter => {
                // Confirm selected option
                let request_id = approval.request_id.clone();
                if let Some(response) = pending_response(approval) {
                    send_and_remove_front(&mut guard, &request_id, response);
                }
                true
            }
            KeyCode::Char('y') if approval.kind == PendingHumanLoopKind::Approval => {
                approval.selected_option = 0;
                let request_id = approval.request_id.clone();
                if let Some(response) = pending_response(approval) {
                    send_and_remove_front(&mut guard, &request_id, response);
                }
                true
            }
            KeyCode::Char('n') if approval.kind == PendingHumanLoopKind::Approval => {
                approval.selected_option = 1;
                approval.input_mode = true;
                approval.input_label = "拒绝原因".to_string();
                approval.feedback_input.clear();
                approval.feedback_cursor = 0;
                true
            }
            KeyCode::Char('m') if approval.kind == PendingHumanLoopKind::Approval => {
                approval.selected_option = 2;
                approval.input_mode = true;
                approval.input_label = "修改意见".to_string();
                approval.feedback_input.clear();
                approval.feedback_cursor = 0;
                true
            }
            KeyCode::Char('a') if approval.kind == PendingHumanLoopKind::Approval => {
                approval.selected_option = 3;
                let request_id = approval.request_id.clone();
                if let Some(response) = pending_response(approval) {
                    send_and_remove_front(&mut guard, &request_id, response);
                }
                true
            }
            KeyCode::Esc => {
                let request_id = approval.request_id.clone();
                send_and_remove_front(
                    &mut guard,
                    &request_id,
                    HumanLoopResponse::Rejected {
                        reason: Some("User dismissed".to_string()),
                    },
                );
                true
            }
            _ => false, // Let other keys through
        }
    }
}

/// Build the response for the currently selected option.
fn pending_response(
    approval: &mut echo_agent_app_core::hitl::PendingApproval,
) -> Option<echo_agent::human_loop::HumanLoopResponse> {
    use echo_agent::human_loop::{ApprovalScope, HumanLoopResponse};
    use echo_agent_app_core::hitl::PendingHumanLoopKind;

    let response = match approval.kind {
        PendingHumanLoopKind::Input => HumanLoopResponse::Text(approval.feedback_input.clone()),
        PendingHumanLoopKind::Selection => {
            let selection = approval.options.get(approval.selected_option).cloned()?;
            HumanLoopResponse::Selection {
                selection,
                instructions: None,
            }
        }
        PendingHumanLoopKind::Approval => match approval.selected_option {
            0 => HumanLoopResponse::Approved,
            1 => {
                approval.input_mode = true;
                approval.input_label = "拒绝原因".to_string();
                approval.feedback_input.clear();
                approval.feedback_cursor = 0;
                return None;
            }
            2 => {
                approval.input_mode = true;
                approval.input_label = "修改意见".to_string();
                approval.feedback_input.clear();
                approval.feedback_cursor = 0;
                return None;
            }
            3 => HumanLoopResponse::ApprovedWithScope {
                scope: ApprovalScope::SessionTool,
            },
            _ => HumanLoopResponse::Approved,
        },
    };

    Some(response)
}

/// Resolve and remove the exact front request before releasing the queue lock.
fn send_and_remove_front(
    pending: &mut echo_agent_app_core::hitl::tui_provider::TuiHumanLoopState,
    request_id: &str,
    response: echo_agent::human_loop::HumanLoopResponse,
) -> bool {
    pending.resolve_front(request_id, response)
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
    ToolBatchStart {
        tool_count: usize,
    },
    /// A tool batch has ended.
    ToolBatchEnd,
    /// The final complete answer from the agent.
    FinalAnswer(String),
    Cancelled,
    /// A tool is about to be called.
    ToolCall {
        call_id: String,
        name: String,
        args: String,
    },
    ToolProgress {
        call_id: String,
        message: String,
    },
    ToolOutput {
        call_id: String,
        channel: ToolOutputChannel,
        chunk: String,
    },
    ToolComplete {
        call_id: String,
        success: bool,
        metadata: std::collections::HashMap<String, String>,
        truncated: bool,
        failure: Option<ToolFailure>,
    },
    /// A tool execution completed.
    ToolResult {
        call_id: String,
        output: String,
        success: bool,
        failure: Option<ToolFailure>,
    },
    /// An error occurred.
    Error(String),
    /// Context was auto-compressed to fit within token limits.
    ContextCompressed {
        before_count: usize,
        after_count: usize,
        before_tokens: usize,
        after_tokens: usize,
    },
    /// Provider-reported LLM usage（透传框架事件，用于上下文窗口占用展示）。
    LlmUsage {
        prompt_tokens: usize,
        completion_tokens: usize,
        cached_prompt_tokens: usize,
        cache_creation_prompt_tokens: usize,
        /// false = provider 未报 usage，勿更新 snapshot / accumulator。
        usage_reported: bool,
    },
    Notice(String),
    Execution(echo_agent_app_core::tasks::task_runtime::executor::ExecEvent),
    TurnStatus(String),
    /// The sole TUI lifecycle terminal, emitted after the driver settles.
    TurnSettled {
        turn_id: String,
        outcome: TurnOutcome,
    },
    ExecutionPath {
        requested_mode: String,
        observed_path: String,
    },
    Interrupt {
        run_id: String,
        goal: String,
        new_message: String,
    },
}

#[derive(Clone, Copy)]
enum ToolOutputChannel {
    Stdout,
    Stderr,
    Log,
}

fn find_tool_mut<'a>(app: &'a mut TuiApp, call_id: &str) -> Option<&'a mut ToolExecutionMessage> {
    app.messages
        .iter_mut()
        .find_map(|message| match &mut message.role {
            MessageRole::ToolExecution(tool) if tool.call_id == call_id => Some(tool.as_mut()),
            _ => None,
        })
}

fn append_bounded(
    target: &mut String,
    chunk: &str,
    max_chars: usize,
    max_bytes: usize,
    max_lines: usize,
) -> bool {
    target.push_str(chunk);
    let mut kept = Vec::new();
    let mut bytes = 0usize;
    let mut lines = 1usize;
    for character in target.chars().rev() {
        let next_lines = lines.saturating_add(usize::from(character == '\n'));
        if kept.len().saturating_add(1) > max_chars
            || bytes.saturating_add(character.len_utf8()) > max_bytes
            || next_lines > max_lines
        {
            break;
        }
        bytes = bytes.saturating_add(character.len_utf8());
        lines = next_lines;
        kept.push(character);
    }
    let truncated = kept.len() < target.chars().count();
    if truncated {
        *target = kept.into_iter().rev().collect();
    }
    truncated
}

#[cfg(test)]
mod tool_execution_tests {
    use super::append_bounded;
    use crate::tui::{
        ToolExecutionMessage, ToolExecutionStatus, tool_command, tool_detail, tool_metadata_label,
        tool_output_tail,
    };
    use std::collections::HashMap;
    use std::time::Instant;

    #[test]
    fn bounded_tool_output_keeps_unicode_tail() {
        let mut output = "开始🙂".to_string();
        append_bounded(&mut output, "结束世界", 5, usize::MAX, usize::MAX);
        assert_eq!(output, "🙂结束世界");
    }

    #[test]
    fn bounded_tool_output_respects_utf8_bytes_and_lines() {
        let mut output = "🙂🙂🙂".to_string();
        assert!(append_bounded(&mut output, "", usize::MAX, 8, usize::MAX));
        assert_eq!(output, "🙂🙂");

        let mut lines = "1\n2\n3\n4".to_string();
        assert!(append_bounded(&mut lines, "", usize::MAX, usize::MAX, 3));
        assert_eq!(lines, "2\n3\n4");
    }

    fn execution(name: &str, args: &str, status: ToolExecutionStatus) -> ToolExecutionMessage {
        ToolExecutionMessage {
            call_id: "call-1".to_string(),
            name: name.to_string(),
            args: args.to_string(),
            status,
            stdout: "result line".to_string(),
            stderr: String::new(),
            log: String::new(),
            progress: None,
            truncated: false,
            started_at: Instant::now(),
            finished_at: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn file_tools_render_compact_titles() {
        assert_eq!(
            tool_command(
                "read_file",
                r#"{"path":"src/main.rs","offset":10,"limit":20}"#
            ),
            "src/main.rs"
        );
        assert_eq!(
            tool_command("edit_file", r#"{"path":"src/lib.rs"}"#),
            "Edit src/lib.rs"
        );
    }

    #[test]
    fn read_detail_uses_requested_line_range() {
        let tool = execution(
            "read_file",
            r#"{"path":"src/main.rs","offset":10,"limit":20}"#,
            ToolExecutionStatus::Succeeded,
        );
        assert_eq!(tool_detail(&tool), "lines 10-29");
        assert!(tool_output_tail(&tool, 6).is_empty());
    }

    #[test]
    fn failed_read_keeps_error_tail() {
        let mut tool = execution(
            "read_file",
            r#"{"path":"missing.rs"}"#,
            ToolExecutionStatus::Failed,
        );
        tool.stderr = "file not found".to_string();
        assert_eq!(
            tool_output_tail(&tool, 6),
            vec!["file not found".to_string()]
        );
    }

    #[test]
    fn missing_artifact_is_visible_without_marking_tool_failed() {
        let mut tool = execution(
            "shell",
            r#"{"command":"large-output"}"#,
            ToolExecutionStatus::Succeeded,
        );
        tool.truncated = true;
        tool.metadata.insert(
            "artifact_path".to_string(),
            "/path/that/does/not/exist/tool.log".to_string(),
        );

        assert!(tool_metadata_label(&tool).contains("artifact missing"));
        assert!(
            tool_output_tail(&tool, 6)
                .iter()
                .any(|line| line.contains("full output artifact missing"))
        );
        assert_eq!(tool.status, ToolExecutionStatus::Succeeded);
    }

    #[test]
    fn search_detail_includes_scope_and_result_count() {
        let mut tool = execution(
            "grep",
            r#"{"pattern":"ToolResult","path":"src"}"#,
            ToolExecutionStatus::Succeeded,
        );
        tool.stdout = "src/lib.rs:10:ToolResult\n\n12 matches found".to_string();
        assert_eq!(tool_detail(&tool), "in src · 12 matches");
    }

    #[test]
    fn browser_and_subagent_tools_render_compact_titles() {
        assert_eq!(
            tool_command(
                "browser_navigate",
                r#"{"url":"https://docs.rs/echo-agent"}"#
            ),
            "Open https://docs.rs/echo-agent"
        );
        assert_eq!(
            tool_command(
                "agent_tool",
                r#"{"agent_name":"reviewer","task":"Review renderer"}"#
            ),
            "Subagent reviewer"
        );
    }

    #[test]
    fn task_tool_detail_reuses_existing_execution_panel_summary() {
        let tool = execution(
            "task_execute",
            r#"{"revision":3}"#,
            ToolExecutionStatus::Succeeded,
        );
        assert_eq!(
            tool_command(&tool.name, &tool.args),
            "Execute task graph r3"
        );
        assert_eq!(tool_detail(&tool), "Committed revision 3");
        assert!(tool_output_tail(&tool, 6).is_empty());
    }

    #[test]
    fn mcp_detail_uses_framework_result_metadata() {
        let mut tool = execution("list_issues", "{}", ToolExecutionStatus::Succeeded);
        tool.metadata
            .insert("tool_source".to_string(), "mcp".to_string());
        tool.metadata
            .insert("mcp_server".to_string(), "github".to_string());
        tool.metadata
            .insert("mcp_tool".to_string(), "list_issues".to_string());
        tool.metadata
            .insert("result_type".to_string(), "json".to_string());
        assert_eq!(tool_detail(&tool), "github · list_issues · json result");
    }

    #[test]
    fn log_channel_remains_distinct_and_visible() {
        let mut tool = execution("custom", "{}", ToolExecutionStatus::Running);
        tool.stdout.clear();
        tool.log = "phase one".to_string();
        assert_eq!(tool_output_tail(&tool, 6), vec!["phase one".to_string()]);
        assert!(tool.stderr.is_empty());
    }
}

/// Run the main event loop.
pub async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
    agent: AgentHandle,
) -> anyhow::Result<()> {
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();
    let mut subagent_event_rx = agent
        .read(|a| a.subagent_registry().event_bus().subscribe())
        .await;
    let mut last_runtime_refresh = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);

    loop {
        while let Ok(event) = subagent_event_rx.try_recv() {
            update_subagent_runs(app, &event);
        }

        if last_runtime_refresh.elapsed() >= Duration::from_millis(250) {
            refresh_task_runtime_view(app);
            last_runtime_refresh = Instant::now();
        }

        // Pre-compute chat area and wrapped lines for mouse selection.
        let size = terminal.size()?;
        let screen = Rect::new(0, 0, size.width, size.height);
        app.chat_area = TuiApp::compute_chat_rect(
            screen,
            app.sidebar_visible,
            app.input_height(screen.width),
            app.parallel_tasks.len().min(5) as u16,
        );
        app.update_wrapped_lines(app.chat_area.width);

        // Flush buffered streaming tokens (throttled to ~2 updates/sec).
        app.flush_pending_stream();

        if app.has_running_tools() {
            app.invalidate_messages_cache();
        }
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
                AgentEvent::LlmUsage {
                    prompt_tokens,
                    completion_tokens,
                    cached_prompt_tokens,
                    cache_creation_prompt_tokens,
                    usage_reported,
                } => {
                    if !usage_reported {
                        tracing::warn!(
                            prompt_tokens,
                            "TUI: LLM usage not reported by provider — skipping snapshot/accumulator update"
                        );
                    } else {
                        // 更新"当前上下文占用"快照（覆盖式，对齐 Claude Code 语义）。
                        // prompt_tokens 是本次请求的真实输入 token（已含 cache 部分）。
                        app.context_snapshot = ContextWindowSnapshot {
                            input_tokens: prompt_tokens as u32,
                            cached_tokens: cached_prompt_tokens as u32,
                            cache_creation_tokens: cache_creation_prompt_tokens as u32,
                            output_tokens: completion_tokens as u32,
                            context_window_size: app.context_window_size,
                            updated_at: Some(std::time::Instant::now()),
                        };
                        app.usage_accumulator.record(
                            prompt_tokens as u64,
                            cached_prompt_tokens as u64,
                            true,
                        );
                    }
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
                AgentEvent::Cancelled => {
                    render_cancelled_event(app);
                }
                AgentEvent::ToolCall {
                    call_id,
                    name,
                    args,
                } => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::ToolExecution(Box::new(ToolExecutionMessage {
                            call_id,
                            name,
                            args,
                            status: ToolExecutionStatus::Running,
                            stdout: String::new(),
                            stderr: String::new(),
                            log: String::new(),
                            progress: None,
                            truncated: false,
                            started_at: Instant::now(),
                            finished_at: None,
                            metadata: std::collections::HashMap::new(),
                        })),
                        content: String::new(),
                    });
                    app.rebuild_message_groups();
                }
                AgentEvent::ToolProgress { call_id, message } => {
                    if let Some(tool) = find_tool_mut(app, &call_id) {
                        tool.progress = Some(message);
                    }
                    app.invalidate_messages_cache();
                }
                AgentEvent::ToolOutput {
                    call_id,
                    channel,
                    chunk,
                } => {
                    if let Some(tool) = find_tool_mut(app, &call_id) {
                        let target = match channel {
                            ToolOutputChannel::Stdout => &mut tool.stdout,
                            ToolOutputChannel::Stderr => &mut tool.stderr,
                            ToolOutputChannel::Log => &mut tool.log,
                        };
                        tool.truncated |= append_bounded(target, &chunk, 131_072, 131_072, 1_000);
                    }
                    app.invalidate_messages_cache();
                }
                AgentEvent::ToolComplete {
                    call_id,
                    success,
                    mut metadata,
                    truncated,
                    failure,
                } => {
                    if let Some(failure) = failure {
                        metadata.insert(
                            "failure_category".to_string(),
                            failure.category.as_str().to_string(),
                        );
                        metadata.insert(
                            "recovery_action".to_string(),
                            failure.recovery.as_str().to_string(),
                        );
                        if let Some(postcondition) = failure.postcondition {
                            metadata.insert("postcondition".to_string(), postcondition);
                        }
                    }
                    if let Some(tool) = find_tool_mut(app, &call_id) {
                        tool.status = if success {
                            ToolExecutionStatus::Succeeded
                        } else {
                            ToolExecutionStatus::Failed
                        };
                        tool.finished_at = Some(Instant::now());
                        tool.truncated |= truncated
                            || metadata
                                .get("output_truncated")
                                .is_some_and(|value| value == "true");
                        tool.metadata = metadata;
                    }
                    app.invalidate_messages_cache();
                }
                AgentEvent::ToolResult {
                    call_id,
                    output,
                    success,
                    failure,
                } => {
                    let mut diff_tool_name = None;
                    if let Some(tool) = find_tool_mut(app, &call_id) {
                        if let Some(failure) = failure {
                            tool.metadata.insert(
                                "failure_category".to_string(),
                                failure.category.as_str().to_string(),
                            );
                            tool.metadata.insert(
                                "recovery_action".to_string(),
                                failure.recovery.as_str().to_string(),
                            );
                            if let Some(postcondition) = failure.postcondition {
                                tool.metadata
                                    .insert("postcondition".to_string(), postcondition);
                            }
                        }
                        tool.status = if success {
                            ToolExecutionStatus::Succeeded
                        } else {
                            ToolExecutionStatus::Failed
                        };
                        tool.finished_at = Some(Instant::now());
                        if matches!(
                            tool.name.as_str(),
                            "edit_file" | "create_file" | "write_file"
                        ) {
                            diff_tool_name = Some(tool.name.clone());
                        }
                        if success && tool.stdout.is_empty() {
                            tool.stdout = output.clone();
                        } else if !success && tool.stderr.is_empty() {
                            tool.stderr = output.clone();
                        }
                    }
                    if let Some(tool_name) = diff_tool_name {
                        app.messages.push(ChatMessage {
                            role: MessageRole::ToolResult { tool_name },
                            content: output,
                        });
                        app.rebuild_message_groups();
                    }
                    app.invalidate_messages_cache();
                }
                AgentEvent::Error(e) => {
                    render_error_event(app, &e);
                }
                AgentEvent::ContextCompressed {
                    before_count,
                    after_count,
                    before_tokens,
                    after_tokens,
                } => {
                    // 方案 A：压缩后 Snapshot 置空，等下一轮 LlmUsage；Accumulator 保留。
                    app.context_snapshot.clear_usage();
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
                AgentEvent::Notice(message) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: message,
                    });
                    app.rebuild_message_groups();
                }
                AgentEvent::Execution(event) => {
                    app.status_msg = format!("{}: {}", event.run_id, event.event);
                    if event.event.is_attention_event() {
                        let detail: String = event.payload.to_string().chars().take(500).collect();
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!("TaskRuntime {}: {}", event.event, detail),
                        });
                        app.rebuild_message_groups();
                    }
                }
                AgentEvent::TurnStatus(status) => {
                    app.status_msg = status;
                }
                AgentEvent::TurnSettled { turn_id, outcome } => {
                    if apply_turn_settlement(app, &turn_id, &outcome) {
                        dispatch_next_queued(app, &agent, agent_tx.clone()).await;
                    }
                }
                AgentEvent::ExecutionPath {
                    requested_mode,
                    observed_path,
                } => {
                    app.status_msg = format!("{requested_mode} -> {observed_path}");
                }
                AgentEvent::Interrupt {
                    run_id,
                    goal,
                    new_message,
                } => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!(
                            "Run {run_id} is paused ({goal}). New instruction: {new_message}"
                        ),
                    });
                    app.rebuild_message_groups();
                }
            }
        }

        if !app.is_processing && !app.queued_turns.is_empty() {
            dispatch_next_queued(app, &agent, agent_tx.clone()).await;
        }

        // Handle events.
        // ── Resilient event reading: tolerate transient I/O errors ──
        // On macOS, terminal resize generates SIGWINCH which can interrupt
        // crossterm's underlying read() syscall with EINTR. We must NOT
        // propagate these as fatal errors — just skip the tick and retry.
        match event::poll(POLL_INTERVAL) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => handle_key(app, key, &agent, agent_tx.clone()).await,
                Ok(Event::Paste(text)) => handle_pasted_text(app, &text),
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

        if app.external_editor_requested {
            app.external_editor_requested = false;
            if let Err(error) = open_external_editor(terminal, app) {
                app.status_msg = format!("External editor failed: {error}");
            }
        }
        if let Some(path) = app.external_file_editor_requested.take() {
            match open_external_file_editor(terminal, app.inline_mode, &path) {
                Ok(()) => {
                    app.status_msg = format!("Saved {}", path.display());
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Edited {} in external editor.", path.display()),
                    });
                    app.rebuild_message_groups();
                }
                Err(error) => {
                    app.status_msg = format!("File editor failed: {error}");
                }
            }
        }
        if app.rewind_requested {
            app.rewind_requested = false;
            if let Err(error) = rewind_last_turn(app, &agent).await {
                app.status_msg = format!("Rewind failed: {error}");
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn render_cancelled_event(app: &mut TuiApp) {
    app.status_msg = "Cancellation acknowledged · settling".to_string();
}

fn render_error_event(app: &mut TuiApp, error: &str) {
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        content: format!("Error: {error}"),
    });
    app.rebuild_message_groups();
    app.status_msg = "Turn ending · awaiting settlement".to_string();
}

/// Apply the one authoritative terminal projection for the current TUI turn.
///
/// Returning `true` is the event loop's only permission to advance the FIFO.
/// Exact id matching makes duplicate and stale settlements harmless.
fn apply_turn_settlement(app: &mut TuiApp, turn_id: &str, outcome: &TurnOutcome) -> bool {
    if app.active_turn_id.as_deref() != Some(turn_id) {
        tracing::debug!(
            settlement_turn_id = turn_id,
            active_turn_id = ?app.active_turn_id.as_deref(),
            "ignoring stale TUI turn settlement"
        );
        return false;
    }

    app.finalize_stream();
    let now = Instant::now();
    for message in &mut app.messages {
        if let MessageRole::ToolExecution(tool) = &mut message.role
            && tool.status == ToolExecutionStatus::Running
        {
            match outcome {
                TurnOutcome::Completed => {
                    tracing::warn!(
                        call_id = %tool.call_id,
                        tool = %tool.name,
                        "completed TUI turn was missing a tool terminal event"
                    );
                    tool.status = ToolExecutionStatus::Failed;
                    tool.finished_at = Some(now);
                    if tool.stderr.is_empty() {
                        tool.stderr =
                            "Tool terminal event missing before completed turn settlement"
                                .to_string();
                    }
                }
                TurnOutcome::Cancelled => {
                    tool.status = ToolExecutionStatus::Cancelled;
                    tool.finished_at = Some(now);
                }
                TurnOutcome::Failed(failure) => {
                    tool.status = ToolExecutionStatus::Failed;
                    tool.finished_at = Some(now);
                    if tool.stderr.is_empty() {
                        tool.stderr = failure.message.clone();
                    }
                }
            }
        }
    }
    app.is_processing = false;
    app.active_turn_id = None;
    app.status_msg = match outcome {
        TurnOutcome::Completed => "Ready".to_string(),
        TurnOutcome::Cancelled => "Cancelled".to_string(),
        TurnOutcome::Failed(failure) => format!("Error: {}", failure.message),
    };
    app.invalidate_messages_cache();
    app.rebuild_message_groups();
    true
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
            guard.as_ref().map(|g| !g.is_empty()).unwrap_or(false)
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

    if let Some(result) = handle_global_shortcuts(app, &key).await
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
async fn handle_global_shortcuts(app: &mut TuiApp, key: &KeyEvent) -> Option<bool> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }

    match key.code {
        KeyCode::Char('c') => {
            if app.is_processing {
                handle_esc(app).await;
            } else if !app.input.is_empty() {
                app.input.clear();
                app.cursor = 0;
                app.update_suggestions();
            } else {
                app.should_quit = true;
            }
            Some(true)
        }
        KeyCode::Char('q') => {
            if app.is_processing
                && let Err(error) = cancel_active_tui_turn(app).await
            {
                app.status_msg = format!("Unable to stop current turn: {error}");
            }
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
        KeyCode::Char('g') => {
            app.external_editor_requested = true;
            Some(true)
        }
        KeyCode::Char('r') => {
            reverse_history_search(app);
            Some(true)
        }
        KeyCode::Char('o') => {
            let collapse = app.message_groups.iter().any(|group| group.collapsed);
            for group in &mut app.message_groups {
                if matches!(
                    group.group_type,
                    super::MessageGroupType::AssistantTurn { .. }
                ) {
                    group.collapsed = !collapse;
                }
            }
            app.status_msg = if collapse {
                "Expanded transcript details".to_string()
            } else {
                "Collapsed transcript details".to_string()
            };
            Some(true)
        }
        KeyCode::Char('v') => {
            paste_clipboard(app);
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
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('a') => {
                app.cursor = current_line_start(&app.input, app.cursor);
                return;
            }
            KeyCode::Char('e') => {
                app.cursor = current_line_end(&app.input, app.cursor);
                return;
            }
            KeyCode::Char('j') => {
                insert_newline(app);
                return;
            }
            KeyCode::Char('u') => {
                let start = current_line_start(&app.input, app.cursor);
                app.input.drain(start..app.cursor);
                app.cursor = start;
                app.update_suggestions();
                return;
            }
            KeyCode::Char('w') => {
                delete_previous_word(app);
                return;
            }
            _ => {}
        }
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Char('b') => {
                app.cursor = previous_word_boundary(&app.input, app.cursor);
                return;
            }
            KeyCode::Char('f') => {
                app.cursor = next_word_boundary(&app.input, app.cursor);
                return;
            }
            _ => {}
        }
    }
    // Shift+Enter: newline
    if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::Enter {
        insert_newline(app);
        return;
    }

    match key.code {
        KeyCode::Enter => handle_enter(app, agent, agent_tx).await,
        KeyCode::Char(c) => handle_char_input(app, c),
        KeyCode::Backspace => handle_backspace(app),
        KeyCode::Delete => handle_delete(app),
        KeyCode::Left => handle_cursor_left(app),
        KeyCode::Right => handle_cursor_right(app),
        KeyCode::Home => app.cursor = current_line_start(&app.input, app.cursor),
        KeyCode::End => app.cursor = current_line_end(&app.input, app.cursor),
        KeyCode::Up => handle_up(app, key),
        KeyCode::Down => handle_down(app, key),
        KeyCode::PageUp => app.chat_scroll = app.chat_scroll.saturating_add(30),
        KeyCode::PageDown => app.chat_scroll = app.chat_scroll.saturating_sub(30),
        KeyCode::Tab if complete_file_reference(app) => {}
        KeyCode::Tab => app.sidebar_tab = (app.sidebar_tab + 1) % 3,
        KeyCode::Esc => handle_esc(app).await,
        _ => {}
    }
}

async fn handle_enter(
    app: &mut TuiApp,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) {
    let text = match app.take_input() {
        Some(text) => text,
        None if !app.pending_attachments.is_empty() => String::new(),
        None => return,
    };
    if text.starts_with('/') {
        if app.is_processing && !slash_command_allowed_while_busy(&text) {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: "Agent 正在运行。请先按 Esc 中断，或等待当前轮结束后再执行该命令。"
                    .to_string(),
            });
            app.rebuild_message_groups();
            return;
        }
        handle_slash_command(app, agent, agent_tx, &text).await;
        return;
    }
    if let Some(command) = text.strip_prefix('!') {
        run_local_shell(app, command.trim()).await;
        return;
    }

    let turn = QueuedTurn {
        text,
        attachments: std::mem::take(&mut app.pending_attachments),
        interaction_mode: app.interaction_mode,
        run_resume: None,
    };
    if app.is_processing {
        let preview: String = turn.text.chars().take(60).collect();
        app.queued_turns.push_back(turn);
        app.status_msg = format!("运行中 · 已排队 {} 条", app.queued_turns.len());
        tracing::info!(queued = app.queued_turns.len(), preview, "TUI turn queued");
        return;
    }
    if let TurnDispatchResult::Rejected { turn, error, .. } =
        dispatch_turn(app, agent, agent_tx, turn).await
    {
        restore_undispatched_turn(app, turn, error);
    }
}

enum TurnDispatchResult {
    Started,
    Rejected {
        turn: QueuedTurn,
        error: String,
        retryable: bool,
    },
}

async fn dispatch_turn(
    app: &mut TuiApp,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
    turn: QueuedTurn,
) -> TurnDispatchResult {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let run_turn_binding = run_turn_binding_for_queued_turn(&turn, &turn_id);
    let sink: std::sync::Arc<dyn echo_agent_app_core::chat_driver::ChatSink> =
        std::sync::Arc::new(TuiChatSink::new(agent_tx.clone()));
    let spill_dir = echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(
        app.workspace_root.as_deref(),
    );
    let prepared = match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::prepared_turn::UserTurnInput {
            text: &turn.text,
            attachments: &turn.attachments,
            spill_dir: &spill_dir,
            conversation_id: app.conversation_id.as_deref(),
            turn_id: Some(&turn_id),
        },
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::warn!(%error, "failed to prepare TUI user turn");
            return TurnDispatchResult::Rejected {
                turn,
                error: format!("Failed to prepare user turn: {error}"),
                retryable: false,
            };
        }
    };
    let task_attachments = prepared.inline_attachment_refs();
    let display_text = if turn.text.is_empty() {
        format!("[{} attachment(s)]", turn.attachments.len())
    } else {
        turn.text.clone()
    };
    let foreground_turns = match app.app_state.as_ref() {
        Some(state) => state.session.foreground_turns.clone(),
        None => {
            return TurnDispatchResult::Rejected {
                turn,
                error: "TUI application state is unavailable".to_string(),
                retryable: false,
            };
        }
    };
    let lease = match begin_tui_foreground_turn(app, &turn_id) {
        Ok(lease) => lease,
        Err(error) => {
            tracing::warn!(%error, turn_id, "failed to acquire TUI foreground turn");
            let retryable = matches!(
                &error,
                echo_agent_app_core::foreground_turn::ForegroundTurnError::Busy { .. }
                    | echo_agent_app_core::foreground_turn::ForegroundTurnError::AdmissionSuspended
            );
            return TurnDispatchResult::Rejected {
                turn,
                error: format!("Unable to start foreground turn: {error}"),
                retryable,
            };
        }
    };
    if let (Some(store), Some(conversation_id)) = (
        app.conversation_store.as_ref(),
        app.conversation_id.as_deref(),
    ) {
        let title: String = turn.text.chars().take(80).collect();
        if let Err(error) = store
            .ensure_conversation(echo_agent::memory::NewConversation {
                conversation_id: conversation_id.to_string(),
                user_id: "default".to_string(),
                agent_type: None,
                title: Some(title),
            })
            .await
        {
            tracing::warn!(error = %error, conversation_id, "failed to ensure TUI conversation metadata");
        }
    }
    let res = std::sync::Arc::new(echo_agent_app_core::chat_resources::ChatResources {
        pool: app.pool.clone(),
        store: app.task_runtime_store.clone(),
        sink,
        webhook_emitter: app.webhook_emitter.clone(),
        // TUI/GUI parity (AGENTS.md): bind this turn to the session's
        // conversation id so TaskRuntime runs + transcript projection work.
        conv_id: app.conversation_id.clone(),
        root_message_id: turn_id.clone(),
        // Bind staged refs so subagents in an autonomous run see them too.
        attachments: task_attachments,
        cancel: lease.cancellation_token(),
        interaction_mode: turn.interaction_mode,
        review_integration: app.review_integration.clone(),
        layer_manager: None,
        memory_generation: None,
        human_loop_provider: app.human_loop_provider.clone().map(|provider| {
            provider as std::sync::Arc<dyn echo_agent::human_loop::HumanLoopProvider>
        }),
    });
    let agent_owned = agent.clone();
    let settled_turn_id = turn_id.clone();
    if let Err(error) = foreground_turns.supervise(lease, move |lease| async move {
        let outcome = match std::panic::AssertUnwindSafe(send_to_agent(
            &agent_owned,
            prepared,
            res,
            lease,
            run_turn_binding,
        ))
        .catch_unwind()
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => {
                tracing::error!(turn_id = %settled_turn_id, "TUI foreground driver panicked");
                TurnOutcome::Cancelled
            }
        };
        let _ = agent_tx.send(AgentEvent::TurnSettled {
            turn_id: settled_turn_id,
            outcome,
        });
    }) {
        return TurnDispatchResult::Rejected {
            turn,
            error: format!("Unable to supervise foreground turn: {error}"),
            retryable: false,
        };
    }
    app.start_turn(&display_text);
    app.active_turn_id = Some(turn_id);
    TurnDispatchResult::Started
}

fn begin_tui_foreground_turn(
    app: &TuiApp,
    turn_id: &str,
) -> Result<ForegroundTurnLease, echo_agent_app_core::foreground_turn::ForegroundTurnError> {
    let app_state = app
        .app_state
        .as_ref()
        .ok_or(echo_agent_app_core::foreground_turn::ForegroundTurnError::StateUnavailable)?;
    let conversation_id = app
        .conversation_id
        .as_deref()
        .ok_or(echo_agent_app_core::foreground_turn::ForegroundTurnError::EmptyConversationId)?;
    app_state
        .session
        .foreground_turns
        .begin(ForegroundTurnSurface::Tui, conversation_id, turn_id)
}

fn restore_undispatched_turn(app: &mut TuiApp, turn: QueuedTurn, error: String) {
    app.pending_attachments.extend(turn.attachments);
    if !turn.text.is_empty() {
        if app.history.last().is_some_and(|entry| entry == &turn.text) {
            app.history.pop();
        }
        app.input = turn.text;
        app.cursor = app.input.len();
        app.update_suggestions();
    }
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        content: error,
    });
    app.status_msg = "Ready".to_string();
    app.rebuild_message_groups();
}

fn active_tui_turn(app: &TuiApp) -> Result<ForegroundTurnSnapshot, String> {
    let app_state = app
        .app_state
        .as_ref()
        .ok_or_else(|| "TUI application state is unavailable".to_string())?;
    let conversation_id = app
        .conversation_id
        .as_deref()
        .ok_or_else(|| "TUI conversation id is unavailable".to_string())?;
    let expected_turn_id = app
        .active_turn_id
        .as_deref()
        .ok_or_else(|| "TUI turn projection is unavailable".to_string())?;
    let snapshot = app_state
        .session
        .foreground_turns
        .snapshot(ForegroundTurnSurface::Tui, conversation_id)
        .ok_or_else(|| "No active TUI foreground turn".to_string())?;
    if snapshot.turn_id != expected_turn_id {
        return Err(format!(
            "TUI foreground turn mismatch: expected {expected_turn_id}, actual {}",
            snapshot.turn_id
        ));
    }
    Ok(snapshot)
}

async fn dispatch_next_queued(
    app: &mut TuiApp,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) {
    let Some(turn) = app.queued_turns.pop_front() else {
        return;
    };
    let result = dispatch_turn(app, agent, agent_tx, turn).await;
    project_queued_dispatch(app, result);
}

fn project_queued_dispatch(app: &mut TuiApp, result: TurnDispatchResult) {
    match result {
        TurnDispatchResult::Started => {}
        TurnDispatchResult::Rejected {
            turn,
            error,
            retryable: true,
        } => {
            app.queued_turns.push_front(turn);
            app.status_msg = format!("Queued turn is waiting for admission: {error}");
        }
        TurnDispatchResult::Rejected {
            error,
            retryable: false,
            ..
        } => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: error,
            });
            app.rebuild_message_groups();
        }
    }
}

fn queue_steer_follow_up(
    app: &mut TuiApp,
    instruction: &str,
    attachments: Vec<echo_agent_app_core::attachments::AttachmentRef>,
) {
    app.queued_turns.push_back(QueuedTurn {
        text: instruction.to_string(),
        attachments,
        interaction_mode: app.interaction_mode,
        run_resume: None,
    });
    app.status_msg = format!(
        "Current stage is not steerable; queued {} follow-up(s)",
        app.queued_turns.len()
    );
}

fn slash_command_allowed_while_busy(text: &str) -> bool {
    let command = text.split_whitespace().next().unwrap_or("");
    matches!(
        command,
        "/help" | "/status" | "/stats" | "/cost" | "/copy" | "/tasks" | "/steer"
    )
}

fn handle_char_input(app: &mut TuiApp, c: char) {
    app.input.insert(app.cursor, c);
    app.cursor += c.len_utf8();
    app.reverse_search_idx = None;
    app.reverse_search_query = None;
    app.update_suggestions();
}

fn insert_text(app: &mut TuiApp, text: &str) {
    app.input.insert_str(app.cursor, text);
    app.cursor = app.cursor.saturating_add(text.len());
    app.reverse_search_idx = None;
    app.reverse_search_query = None;
    app.update_suggestions();
}

fn handle_pasted_text(app: &mut TuiApp, text: &str) {
    if text.chars().count() < PASTE_ATTACHMENT_CHAR_THRESHOLD {
        insert_text(app, text);
        return;
    }
    use base64::Engine as _;
    use echo_agent_app_core::types::{AttachmentData, AttachmentSource};

    let data = AttachmentData {
        name: format!(
            "pasted-text-{}.txt",
            app.pending_attachments.len().saturating_add(1)
        ),
        mime_type: "text/plain".to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(text.as_bytes()),
        size: u64::try_from(text.len()).unwrap_or(u64::MAX),
        source: AttachmentSource::Paste,
    };
    match echo_agent_app_core::attachments::stage_attachment_data(
        &data,
        app.workspace_root.as_deref(),
    ) {
        Ok(reference) => {
            app.pending_attachments.push(reference);
            app.status_msg = format!(
                "Pasted text attached · {} chars · {} resource(s) staged",
                text.chars().count(),
                app.pending_attachments.len()
            );
        }
        Err(error) => {
            insert_text(app, text);
            app.status_msg = format!("Failed to stage pasted text: {error}");
        }
    }
}

fn paste_clipboard(app: &mut TuiApp) {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(error) => {
            app.status_msg = format!("Clipboard unavailable: {error}");
            return;
        }
    };
    if let Ok(image) = clipboard.get_image() {
        let width = match u32::try_from(image.width) {
            Ok(value) => value,
            Err(_) => {
                app.status_msg = "Clipboard image width is unsupported".to_string();
                return;
            }
        };
        let height = match u32::try_from(image.height) {
            Ok(value) => value,
            Err(_) => {
                app.status_msg = "Clipboard image height is unsupported".to_string();
                return;
            }
        };
        let path = std::env::temp_dir().join(format!("eko-clipboard-{}.png", uuid::Uuid::new_v4()));
        let saved = image::save_buffer_with_format(
            &path,
            image.bytes.as_ref(),
            width,
            height,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        );
        match saved.and_then(|()| {
            stage_attachment(
                &mut app.pending_attachments,
                &path,
                app.workspace_root.as_deref(),
            )
            .map(|_| ())
            .map_err(image::ImageError::IoError)
        }) {
            Ok(()) => {
                app.status_msg = format!(
                    "Clipboard image attached · {} file(s) staged",
                    app.pending_attachments.len()
                );
            }
            Err(error) => app.status_msg = format!("Clipboard image attach failed: {error}"),
        }
        let _ = std::fs::remove_file(path);
        return;
    }
    match clipboard.get_text() {
        Ok(text) => handle_pasted_text(app, &text),
        Err(error) => app.status_msg = format!("Clipboard has no supported content: {error}"),
    }
}

fn reverse_history_search(app: &mut TuiApp) {
    if app.history.is_empty() {
        app.status_msg = "No input history".to_string();
        return;
    }
    let query = app
        .reverse_search_query
        .clone()
        .unwrap_or_else(|| app.input.clone());
    app.reverse_search_query = Some(query.clone());
    let start = app.reverse_search_idx.unwrap_or(app.history.len());
    let found = app
        .history
        .get(..start)
        .and_then(|items| {
            items
                .iter()
                .enumerate()
                .rev()
                .find(|(_, item)| query.is_empty() || item.contains(&query))
        })
        .map(|(index, item)| (index, item.clone()));
    match found {
        Some((index, item)) => {
            app.input = item;
            app.cursor = app.input.len();
            app.reverse_search_idx = Some(index);
            app.status_msg = format!("reverse-i-search: {}", app.input);
            app.update_suggestions();
        }
        None => app.status_msg = format!("No earlier history match for: {query}"),
    }
}

fn complete_file_reference(app: &mut TuiApp) -> bool {
    let Some(prefix) = app.input.get(..app.cursor) else {
        return false;
    };
    let token_start = prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(index, ch)| index.saturating_add(ch.len_utf8()))
        .unwrap_or(0);
    let Some(token) = prefix.get(token_start..) else {
        return false;
    };
    let Some(query) = token.strip_prefix('@') else {
        return false;
    };
    let query_lower = query.to_ascii_lowercase();
    let Some(path) = app
        .project_files
        .iter()
        .find(|path| path.to_ascii_lowercase().contains(&query_lower) && path.as_str() != query)
    else {
        app.status_msg = format!("No file match for @{query}");
        return true;
    };
    app.input
        .replace_range(token_start..app.cursor, &format!("@{path}"));
    app.cursor = token_start.saturating_add(1).saturating_add(path.len());
    app.status_msg = format!("Referenced file: {path}");
    true
}

async fn run_local_shell(app: &mut TuiApp, command: &str) {
    if command.is_empty() {
        app.messages.push(ChatMessage {
            role: MessageRole::System,
            content: "Usage: !<shell command>".to_string(),
        });
        return;
    }
    app.status_msg = format!("Running shell: {command}");
    let result = tokio::process::Command::new("sh")
        .arg("-lc")
        .arg(command)
        .output()
        .await;
    let content = match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let status = output.status.code().map_or_else(
                || "terminated by signal".to_string(),
                |code| format!("exit {code}"),
            );
            format!("$ {command}\n[{status}]\n{stdout}{stderr}")
        }
        Err(error) => format!("$ {command}\nFailed to execute: {error}"),
    };
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        content,
    });
    app.status_msg = "Ready".to_string();
    app.rebuild_message_groups();
}

fn open_external_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> anyhow::Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let path = std::env::temp_dir().join(format!("eko-prompt-{}.md", uuid::Uuid::new_v4()));
    std::fs::write(&path, &app.input)?;

    let status = run_external_editor(terminal, app.inline_mode, &path);
    let edited = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    status?
        .success()
        .then_some(())
        .ok_or_else(|| anyhow::anyhow!("editor '{editor}' exited unsuccessfully"))?;
    app.input = edited?;
    app.cursor = app.input.len();
    app.update_suggestions();
    app.status_msg = "Prompt updated from external editor".to_string();
    Ok(())
}

fn open_external_file_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    inline_mode: bool,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let status = run_external_editor(terminal, inline_mode, path)?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| anyhow::anyhow!("editor '{editor}' exited unsuccessfully"))
}

fn run_external_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    inline_mode: bool,
    path: &std::path::Path,
) -> anyhow::Result<std::process::ExitStatus> {
    disable_raw_mode()?;
    execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture)?;
    if !inline_mode {
        execute!(io::stdout(), LeaveAlternateScreen)?;
    }
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg("${VISUAL:-${EDITOR:-vi}} \"$1\"")
        .arg("eko-editor")
        .arg(path)
        .status();
    enable_raw_mode()?;
    if inline_mode {
        execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    } else {
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
    }
    terminal.clear()?;
    status.map_err(Into::into)
}

fn handle_backspace(app: &mut TuiApp) {
    if app.cursor > 0 {
        let prev = app
            .input
            .get(..app.cursor)
            .unwrap_or_default()
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
        let next = app
            .input
            .get(cur..)
            .unwrap_or_default()
            .char_indices()
            .nth(1)
            .map(|(i, _)| cur + i)
            .unwrap_or(app.input.len());
        app.input.drain(cur..next);
    }
}

fn handle_cursor_left(app: &mut TuiApp) {
    if app.cursor > 0 {
        let prev = app
            .input
            .get(..app.cursor)
            .unwrap_or_default()
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
        let ch_len = app
            .input
            .get(cur..)
            .unwrap_or_default()
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
    } else if !move_cursor_vertical(app, -1) {
        app.history_up();
    }
}

fn handle_down(app: &mut TuiApp, key: &KeyEvent) {
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        app.chat_scroll = app.chat_scroll.saturating_sub(10);
    } else if !move_cursor_vertical(app, 1) {
        app.history_down();
    }
}

fn insert_newline(app: &mut TuiApp) {
    app.input.insert(app.cursor, '\n');
    app.cursor = app.cursor.saturating_add(1);
    app.update_suggestions();
}

fn current_line_start(text: &str, cursor: usize) -> usize {
    text.get(..cursor)
        .and_then(|prefix| prefix.rfind('\n').map(|idx| idx.saturating_add(1)))
        .unwrap_or(0)
}

fn current_line_end(text: &str, cursor: usize) -> usize {
    text.get(cursor..)
        .and_then(|suffix| suffix.find('\n').map(|idx| cursor.saturating_add(idx)))
        .unwrap_or(text.len())
}

fn move_cursor_vertical(app: &mut TuiApp, direction: i8) -> bool {
    let line_start = current_line_start(&app.input, app.cursor);
    let column = app
        .input
        .get(line_start..app.cursor)
        .map(|s| s.chars().count())
        .unwrap_or(0);
    let target = if direction < 0 {
        if line_start == 0 {
            return false;
        }
        let previous_end = line_start.saturating_sub(1);
        let previous_start = current_line_start(&app.input, previous_end);
        (previous_start, previous_end)
    } else {
        let line_end = current_line_end(&app.input, app.cursor);
        if line_end >= app.input.len() {
            return false;
        }
        let next_start = line_end.saturating_add(1);
        (next_start, current_line_end(&app.input, next_start))
    };
    app.cursor = app
        .input
        .get(target.0..target.1)
        .and_then(|line| {
            line.char_indices()
                .nth(column)
                .map(|(idx, _)| target.0 + idx)
        })
        .unwrap_or(target.1);
    true
}

fn previous_word_boundary(text: &str, cursor: usize) -> usize {
    let Some(prefix) = text.get(..cursor) else {
        return cursor;
    };
    let mut seen_word = false;
    for (idx, ch) in prefix.char_indices().rev() {
        if ch.is_whitespace() {
            if seen_word {
                return idx.saturating_add(ch.len_utf8());
            }
        } else {
            seen_word = true;
        }
    }
    0
}

fn next_word_boundary(text: &str, cursor: usize) -> usize {
    let Some(suffix) = text.get(cursor..) else {
        return cursor;
    };
    let mut seen_word = false;
    for (idx, ch) in suffix.char_indices() {
        if ch.is_whitespace() {
            if seen_word {
                return cursor.saturating_add(idx);
            }
        } else {
            seen_word = true;
        }
    }
    text.len()
}

fn delete_previous_word(app: &mut TuiApp) {
    let start = previous_word_boundary(&app.input, app.cursor);
    app.input.drain(start..app.cursor);
    app.cursor = start;
    app.update_suggestions();
}

async fn handle_esc(app: &mut TuiApp) {
    if app.is_processing {
        if let Err(error) = cancel_active_tui_turn(app).await {
            app.status_msg = format!("Unable to cancel current turn: {error}");
        }
    } else {
        let now = Instant::now();
        let double_press = app
            .last_escape_at
            .is_some_and(|previous| now.duration_since(previous) <= Duration::from_millis(800));
        if double_press {
            app.rewind_requested = true;
            app.last_escape_at = None;
        } else {
            app.last_escape_at = Some(now);
            app.status_msg = "Press Esc again to rewind the last turn".to_string();
        }
    }
}

async fn cancel_active_tui_turn(app: &mut TuiApp) -> Result<(), String> {
    let snapshot = active_tui_turn(app)?;
    let control = app
        .app_state
        .as_ref()
        .ok_or_else(|| "TUI application state is unavailable".to_string())?
        .session
        .foreground_turns
        .clone();
    app.status_msg = "Cancelling...".to_string();
    control
        .cancel_and_wait(
            ForegroundTurnSurface::Tui,
            &snapshot.conversation_id,
            &snapshot.turn_id,
        )
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

async fn rewind_last_turn(app: &mut TuiApp, agent: &AgentHandle) -> anyhow::Result<()> {
    let store = app
        .conversation_store
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("conversation persistence is unavailable"))?;
    let conversation_id = app
        .conversation_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no active conversation"))?;
    let mut stored = store.get_messages(conversation_id).await?;
    let user_index = stored
        .iter()
        .rposition(|message| message.role == "user")
        .ok_or_else(|| anyhow::anyhow!("no user turn to rewind"))?;
    let prompt = stored
        .get(user_index)
        .and_then(|message| message.content.clone())
        .unwrap_or_default();
    stored.truncate(user_index);
    store.save_messages(conversation_id, &stored).await?;
    let runtime_messages = match echo_agent::memory::restore_messages(&stored) {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::warn!(error = %e, "failed to restore messages; continuing with empty history");
            Vec::new()
        }
    };
    agent
        .read_async(|value| Box::pin(async move { value.load_messages(runtime_messages).await }))
        .await;
    app.input = prompt;
    app.cursor = app.input.len();
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
    app.status_msg = "Last turn rewound into the editor".to_string();
    app.rebuild_message_groups();
    app.update_suggestions();
    Ok(())
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
    fn on_event(&self, event: echo_agent_app_core::chat_driver::ChatDriverEvent) -> bool {
        use echo_agent_app_core::chat_driver::ChatDriverEvent;

        let mapped = match event {
            ChatDriverEvent::Execution(event) => AgentEvent::Execution(event),
            ChatDriverEvent::TurnStatus { status } => AgentEvent::TurnStatus(status),
            ChatDriverEvent::ExecutionPath {
                requested_mode,
                observed_path,
            } => AgentEvent::ExecutionPath {
                requested_mode,
                observed_path,
            },
            ChatDriverEvent::Interrupt {
                run_id,
                goal,
                new_message,
            } => AgentEvent::Interrupt {
                run_id,
                goal,
                new_message,
            },
            ChatDriverEvent::Agent(event) => match event.payload {
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
                echo_agent::agent::AgentEvent::FinalAnswer(answer) => {
                    AgentEvent::FinalAnswer(answer)
                }
                echo_agent::agent::AgentEvent::Cancelled => AgentEvent::Cancelled,
                echo_agent::agent::AgentEvent::ToolCall {
                    call_id,
                    name,
                    args,
                } => AgentEvent::ToolCall {
                    call_id,
                    name,
                    args: args.to_string(),
                },
                echo_agent::agent::AgentEvent::ToolStream {
                    call_id,
                    event:
                        echo_agent::tools::ToolStreamEvent::Progress {
                            message,
                            percent: _,
                        },
                    ..
                } => AgentEvent::ToolProgress { call_id, message },
                echo_agent::agent::AgentEvent::ToolStream {
                    call_id,
                    event: echo_agent::tools::ToolStreamEvent::Output { channel, chunk },
                    ..
                } => AgentEvent::ToolOutput {
                    call_id,
                    channel: match channel {
                        echo_agent::tools::ToolOutputChannel::Stdout => ToolOutputChannel::Stdout,
                        echo_agent::tools::ToolOutputChannel::Stderr => ToolOutputChannel::Stderr,
                        echo_agent::tools::ToolOutputChannel::Log => ToolOutputChannel::Log,
                    },
                    chunk,
                },
                echo_agent::agent::AgentEvent::ToolStream {
                    call_id,
                    event: echo_agent::tools::ToolStreamEvent::Complete(result),
                    ..
                } => AgentEvent::ToolComplete {
                    call_id,
                    success: result.success,
                    metadata: result.metadata,
                    truncated: result.truncated,
                    failure: result.failure,
                },
                echo_agent::agent::AgentEvent::ToolResult {
                    call_id, output, ..
                } => AgentEvent::ToolResult {
                    call_id,
                    output,
                    success: true,
                    failure: None,
                },
                echo_agent::agent::AgentEvent::ToolError {
                    call_id,
                    error,
                    failure,
                    ..
                } => AgentEvent::ToolResult {
                    call_id,
                    output: error,
                    success: false,
                    failure: Some(failure),
                },
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
                echo_agent::agent::AgentEvent::LlmUsage {
                    prompt_tokens,
                    completion_tokens,
                    cached_prompt_tokens,
                    cache_creation_prompt_tokens,
                    usage_reported,
                    ..
                } => {
                    // 透传给主循环：snapshot 更新需要 &mut TuiApp，主循环才拿得到。
                    // （sink 这里只有 &self，无法更新 app 状态。）
                    tracing::debug!(
                        prompt_tokens,
                        cached_prompt_tokens,
                        cache_creation_prompt_tokens,
                        usage_reported,
                        "TUI: LLM usage — forwarding to main loop for context snapshot"
                    );
                    AgentEvent::LlmUsage {
                        prompt_tokens,
                        completion_tokens,
                        cached_prompt_tokens,
                        cache_creation_prompt_tokens,
                        usage_reported,
                    }
                }
                echo_agent::agent::AgentEvent::BudgetDecision {
                    decision,
                    reason,
                    iteration,
                    ..
                } => AgentEvent::Notice(format!(
                    "Budget {decision:?} at iteration {iteration}: {reason}"
                )),
                echo_agent::agent::AgentEvent::GuardTriggered { guard, blocked } => {
                    AgentEvent::Notice(format!("Guard {guard} triggered (blocked={blocked})"))
                }
                echo_agent::agent::AgentEvent::MemoryRecalled { count } => {
                    AgentEvent::Notice(format!("Recalled {count} memory item(s)"))
                }
                echo_agent::agent::AgentEvent::Chart { spec } => {
                    let preview: String = spec.to_string().chars().take(500).collect();
                    AgentEvent::Notice(format!("Chart specification: {preview}"))
                }
                echo_agent::agent::AgentEvent::SafetyNotice {
                    action,
                    reason,
                    risk,
                    permission,
                } => AgentEvent::Notice(format!(
                    "Safety: {action}: {reason} (risk={risk}, permission={permission})"
                )),
                echo_agent::agent::AgentEvent::ParameterError {
                    tool,
                    parameter,
                    expected,
                    got,
                } => AgentEvent::Notice(format!(
                    "Parameter error: {tool}.{parameter} expected {expected}, got {got}"
                )),
                other => AgentEvent::Notice(format!("Agent event: {other:?}")),
            },
        };
        // If the UI dropped the receiver, stop streaming.
        self.tx.send(mapped).is_ok()
    }
}

async fn send_to_agent(
    agent: &AgentHandle,
    turn: echo_agent_app_core::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<echo_agent_app_core::chat_resources::ChatResources>,
    lease: ForegroundTurnLease,
    run_turn_binding: Option<echo_agent_app_core::tasks::task_runtime::RunTurnBinding>,
) -> TurnOutcome {
    use echo_agent_app_core::foreground_turn::{drive_foreground_chat, drive_foreground_chat_turn};

    // TUI does not classify chat versus task locally. The shared foreground
    // driver owns TaskRuntime, memory-generation, and pool admission, while
    // PreparedUserTurn preserves the same staged attachment path as GUI.
    let result = if let Some(binding) = run_turn_binding {
        drive_foreground_chat_turn(lease, agent, &turn, res, binding).await
    } else {
        drive_foreground_chat(lease, agent, &turn, res).await
    };
    result.unwrap_or_else(|error| {
        tracing::warn!(%error, "TUI foreground chat failed");
        TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
            "foreground_turn",
            error,
        ))
    })
}

fn run_turn_binding_for_queued_turn(
    turn: &QueuedTurn,
    turn_id: &str,
) -> Option<echo_agent_app_core::tasks::task_runtime::RunTurnBinding> {
    turn.run_resume.as_ref().map(|resume| {
        echo_agent_app_core::tasks::task_runtime::RunTurnBinding::resume(
            resume.run_id.clone(),
            turn_id.to_string(),
            resume.root_message_id.clone(),
        )
    })
}

async fn handle_tui_cron(app: &TuiApp, args: &str) -> String {
    use echo_agent_app_core::scheduler::{CronTask, CronTaskStatus};

    let Some(runner) = app.scheduler.as_ref() else {
        return "Scheduler is not available in this runtime.".to_string();
    };
    let tokens = match shell_words::split(args) {
        Ok(tokens) => tokens,
        Err(error) => return format!("Invalid cron command: {error}"),
    };
    let subcommand = tokens.first().map(String::as_str).unwrap_or("list");
    let tail = tokens.get(1..).unwrap_or_default();
    match subcommand {
        "list" | "ls" => {
            let tasks = runner.list_tasks().await;
            if tasks.is_empty() {
                return "No scheduled tasks.".to_string();
            }
            tasks
                .iter()
                .map(|task| {
                    format!(
                        "[{}] {} | {} | {}",
                        short_identifier(&task.id),
                        task.name,
                        task.cron_expr,
                        match task.status {
                            CronTaskStatus::Enabled => "enabled",
                            CronTaskStatus::Disabled => "paused",
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        "create" | "add" | "new" => {
            let Some(expression) = tail.first() else {
                return "Usage: /cron create \"*/5 * * * *\" <name> <prompt>".to_string();
            };
            let Some(name) = tail.get(1) else {
                return "Usage: /cron create \"*/5 * * * *\" <name> <prompt>".to_string();
            };
            let prompt = tail.get(2..).unwrap_or_default().join(" ");
            if prompt.trim().is_empty() {
                return "Usage: /cron create \"*/5 * * * *\" <name> <prompt>".to_string();
            }
            if let Err(error) = validate_tui_cron_expression(expression) {
                return format!("Invalid cron expression: {error}");
            }
            let task = CronTask::new(name, expression, &prompt);
            let id = task.id.clone();
            match runner.add_task(task).await {
                Ok(()) => format!("Created cron task {name} [{}].", short_identifier(&id)),
                Err(error) => format!("Failed to create cron task: {error}"),
            }
        }
        "delete" | "remove" | "rm" => mutate_tui_cron_task(runner, tail.first(), "delete").await,
        "pause" | "disable" => mutate_tui_cron_task(runner, tail.first(), "pause").await,
        "resume" | "enable" => mutate_tui_cron_task(runner, tail.first(), "resume").await,
        "run" | "trigger" => mutate_tui_cron_task(runner, tail.first(), "run").await,
        "reload" => match runner.reload().await {
            Ok(count) => format!("Reloaded {count} scheduled task(s)."),
            Err(error) => format!("Failed to reload scheduled tasks: {error}"),
        },
        _ => "Usage: /cron [list|create|delete|pause|resume|run|reload]".to_string(),
    }
}

async fn handle_tui_worktrees(app: &TuiApp, args: &str) -> String {
    use echo_agent_app_core::tasks::task_runtime::worktree::{
        cleanup_unattended_worktrees, discard_unattended_worktree, git_repo_root,
        list_unattended_worktrees, merge_unattended_worktree, repo_merge_lock,
    };

    let tokens = match shell_words::split(args) {
        Ok(tokens) => tokens,
        Err(error) => return format!("Invalid worktree command: {error}"),
    };
    let subcommand = tokens.first().map(String::as_str).unwrap_or("list");
    let run_id = tokens.get(1).cloned();
    let current_dir = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => return format!("Failed to resolve the current workspace: {error}"),
    };
    let repo_root = match git_repo_root(&current_dir) {
        Ok(path) => path,
        Err(error) => return format!("Current workspace is not a Git repository: {error}"),
    };
    let store = app.task_runtime_store.clone();

    match subcommand {
        "list" | "ls" => {
            let result = tokio::task::spawn_blocking(move || {
                list_unattended_worktrees(&repo_root, store.as_deref())
            })
            .await;
            match result {
                Ok(Ok(worktrees)) => format_unattended_worktrees(&worktrees),
                Ok(Err(error)) => format!("Failed to list retained worktrees: {error}"),
                Err(error) => format!("Failed to join worktree listing: {error}"),
            }
        }
        "cleanup" | "clean" => {
            let lock = repo_merge_lock(&repo_root);
            let _guard = lock.lock().await;
            let result = tokio::task::spawn_blocking(move || {
                cleanup_unattended_worktrees(&repo_root, store.as_deref())
            })
            .await;
            match result {
                Ok(Ok(result)) => format!(
                    "Worktree cleanup: removed={}, unlocked={}, kept={}, errors={}{}",
                    result.removed.len(),
                    result.unlocked.len(),
                    result.kept.len(),
                    result.errors.len(),
                    if result.errors.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", result.errors.join("\n"))
                    }
                ),
                Ok(Err(error)) => format!("Failed to clean retained worktrees: {error}"),
                Err(error) => format!("Failed to join worktree cleanup: {error}"),
            }
        }
        "merge" | "integrate" => {
            let Some(run_id) = run_id else {
                return "Usage: /worktrees merge <run-id>".to_string();
            };
            let lock = repo_merge_lock(&repo_root);
            let _guard = lock.lock().await;
            let run_id_for_merge = run_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                merge_unattended_worktree(&repo_root, &run_id_for_merge, store.as_deref())
            })
            .await;
            match result {
                Ok(Ok(outcome)) => {
                    let files = if outcome.changed_files.is_empty() {
                        "none".to_string()
                    } else {
                        outcome.changed_files.join(", ")
                    };
                    let warning = outcome
                        .cleanup_warning
                        .map(|warning| format!("\nCleanup warning: {warning}"))
                        .unwrap_or_default();
                    format!(
                        "Worktree {run_id}: {}. Files: {files}{warning}",
                        outcome.status.as_str()
                    )
                }
                Ok(Err(error)) => format!("Failed to merge retained worktree: {error}"),
                Err(error) => format!("Failed to join worktree merge: {error}"),
            }
        }
        "discard" | "remove" | "rm" => {
            let Some(run_id) = run_id else {
                return "Usage: /worktrees discard <run-id>".to_string();
            };
            let lock = repo_merge_lock(&repo_root);
            let _guard = lock.lock().await;
            let run_id_for_discard = run_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                discard_unattended_worktree(&repo_root, &run_id_for_discard, store.as_deref())
            })
            .await;
            match result {
                Ok(Ok(())) => format!("Discarded retained worktree for run {run_id}."),
                Ok(Err(error)) => format!("Failed to discard retained worktree: {error}"),
                Err(error) => format!("Failed to join worktree discard: {error}"),
            }
        }
        _ => "Usage: /worktrees [list|cleanup|merge <run-id>|discard <run-id>]".to_string(),
    }
}

fn format_unattended_worktrees(
    worktrees: &[echo_agent_app_core::tasks::task_runtime::worktree::UnattendedWorktreeInfo],
) -> String {
    if worktrees.is_empty() {
        return "No retained EKO unattended worktrees.".to_string();
    }
    worktrees
        .iter()
        .map(|worktree| {
            let mut flags = Vec::new();
            flags.push(if worktree.has_changes {
                "changes"
            } else {
                "unchanged"
            });
            if worktree.active {
                flags.push("active");
            }
            if worktree.locked && !worktree.active {
                flags.push("stale-lock");
            }
            if worktree.orphan_branch {
                flags.push("orphan-branch");
            }
            let path = worktree
                .path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "no checkout".to_string());
            format!(
                "{} | {} | {} | {} | {}",
                worktree.run_id,
                flags.join(","),
                worktree.head,
                worktree.status,
                path
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn mutate_tui_cron_task(
    runner: &echo_agent_app_core::scheduler::SchedulerRunner,
    prefix: Option<&String>,
    operation: &str,
) -> String {
    use echo_agent_app_core::scheduler::CronTaskStatus;

    let Some(prefix) = prefix.map(String::as_str) else {
        return format!("Usage: /cron {operation} <id>");
    };
    let tasks = runner.list_tasks().await;
    let matches = tasks
        .iter()
        .filter(|task| task.id.starts_with(prefix))
        .collect::<Vec<_>>();
    let Some(task) = matches.first().copied() else {
        return format!("No cron task matches {prefix}.");
    };
    if matches.len() > 1 {
        let ids = matches
            .iter()
            .map(|task| short_identifier(&task.id))
            .collect::<Vec<_>>()
            .join(", ");
        return format!("Ambiguous cron task prefix; matches: {ids}");
    }
    match operation {
        "delete" => match runner.remove_task(&task.id).await {
            Ok(true) => format!("Deleted cron task {}.", task.name),
            Ok(false) => "Cron task was already removed.".to_string(),
            Err(error) => format!("Failed to delete cron task: {error}"),
        },
        "pause" => match runner.set_status(&task.id, CronTaskStatus::Disabled).await {
            Ok(true) => format!("Paused cron task {}.", task.name),
            Ok(false) => "Cron task was not found.".to_string(),
            Err(error) => format!("Failed to pause cron task: {error}"),
        },
        "resume" => match runner.set_status(&task.id, CronTaskStatus::Enabled).await {
            Ok(true) => format!("Resumed cron task {}.", task.name),
            Ok(false) => "Cron task was not found.".to_string(),
            Err(error) => format!("Failed to resume cron task: {error}"),
        },
        "run" => match runner.run_once(&task.id).await {
            Ok(result) => {
                let preview: String = result.chars().take(1_000).collect();
                format!("Cron task {} finished:\n{preview}", task.name)
            }
            Err(error) => format!("Cron task {} failed: {error}", task.name),
        },
        _ => format!("Unsupported cron operation: {operation}"),
    }
}

fn validate_tui_cron_expression(expression: &str) -> Result<(), String> {
    use std::str::FromStr;

    if expression.split_whitespace().count() != 5 {
        return Err("expected five fields".to_string());
    }
    cron::Schedule::from_str(&format!("0 {expression} *"))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn short_identifier(value: &str) -> String {
    value.chars().take(8).collect()
}

// ── Slash command handling ────────────────────────────────────────────

/// Stage a local file as an attachment for the next TUI message (B5.3).
///
/// Reads `path`, infers a MIME type from the extension, copies the file into
/// the active workspace uploads dir (or the global fallback), and appends an
/// [`AttachmentRef`] to `out`. The caller
/// (`handle_enter`) rebuilds a multimodal `Message` from the refs and passes it
/// to `drive_chat`. Returns the display name + inferred MIME on success.
fn stage_attachment(
    out: &mut Vec<echo_agent_app_core::attachments::AttachmentRef>,
    path: &std::path::Path,
    workspace_root: Option<&std::path::Path>,
) -> std::io::Result<(String, String)> {
    use echo_agent_app_core::attachments::stage_local_attachment;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no filename")
        })?;
    let reference = stage_local_attachment(path, workspace_root).map_err(std::io::Error::other)?;
    let mime = reference.mime_type.clone();
    out.push(reference);
    Ok((name, mime))
}

fn open_artifact_path(path: &std::path::Path) -> Result<(), String> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| format!("Tool-output artifact is missing: {error}"))?;

    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&canonical).spawn();

    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open")
        .arg(&canonical)
        .spawn();

    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer")
        .arg(&canonical)
        .spawn();

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err("Opening local artifacts is unsupported on this platform".to_string());

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    result
        .map(|_| ())
        .map_err(|error| format!("Failed to open tool-output artifact: {error}"))
}

/// Handle slash commands locally in the TUI.
fn push_system_message(app: &mut TuiApp, content: String) {
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        content,
    });
}

async fn refresh_workspace_generation(
    app: &mut TuiApp,
    agent: &AgentHandle,
    state: &echo_agent_app_core::state::AppState,
) {
    app.pending_attachments.clear();
    app.queued_turns.clear();
    app.task_runtime_view = None;
    app.subagent_runs.clear();
    app.workspace_root = state
        .current_workspace()
        .await
        .map(|workspace| workspace.root);
    app.conversation_store = state.storage.conversation_store.read().await.clone();
    app.conversation_id = agent
        .read(|value| value.conversation_id().map(str::to_string))
        .await;
    app.messages.clear();
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        content: "Workspace generation changed; started a new conversation.".to_string(),
    });
    app.project_files = super::collect_project_files(
        app.workspace_root
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new(".")),
        10_000,
    );
    app.rebuild_message_groups();
}

async fn handle_slash_command(
    app: &mut TuiApp,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
    cmd: &str,
) {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let command = parts.first().copied().unwrap_or_default().to_lowercase();
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
            help.push_str("    Ctrl+C             Interrupt / clear draft / quit when empty\n");
            help.push_str("    Ctrl+Q             Quit\n");
            help.push_str("    Ctrl+B             Toggle sidebar\n");
            help.push_str("    Ctrl+L             Clear chat\n");
            help.push_str("    Ctrl+G             Edit prompt in $VISUAL/$EDITOR\n");
            help.push_str("    Ctrl+R             Reverse-search input history\n");
            help.push_str("    Ctrl+O             Toggle transcript details\n");
            help.push_str("    Ctrl+V             Paste text or attach clipboard image\n");
            help.push_str("    Shift+Enter/Ctrl+J Newline in input\n");
            help.push_str("    Ctrl+A / Ctrl+E    Start/end of current line\n");
            help.push_str("    Ctrl+U / Ctrl+W    Delete to line start / previous word\n");
            help.push_str("    Alt+B / Alt+F      Move by word\n");
            help.push_str("    Esc                Cancel generation\n");
            help.push_str("    Tab                Cycle sidebar tabs\n");
            help.push_str("    Up/Down            Navigate input history\n");
            help.push_str("    Shift+Up/Down      Scroll chat\n");
            help.push_str("    PageUp/PageDown    Scroll chat faster\n");
            help.push_str("    Mouse wheel        Scroll chat\n");
            help.push_str("    !command           Run a local shell command\n");
            help.push_str("    @path + Tab        Complete a project file reference\n");

            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: help,
            });
        }
        Some(SlashCommand::Model) => {
            if args.is_empty() {
                let configured = app
                    .configured_models
                    .iter()
                    .map(|model| {
                        format!("  {}  {} ({})", model.id, model.display_name, model.model)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: if configured.is_empty() {
                        format!(
                            "Current model: {}\nNo configured model alternatives.",
                            app.model
                        )
                    } else {
                        format!(
                            "Current model: {}\nConfigured models:\n{}",
                            app.model, configured
                        )
                    },
                });
            } else {
                let requested = args.trim().to_string();
                let Some(app_state) = app.app_state.clone() else {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: "Model selection is unavailable in this runtime.".to_string(),
                    });
                    return;
                };
                let mutation = match app_state.set_default_model_owned(requested).await {
                    Ok(mutation) => mutation,
                    Err(error) => {
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: error.to_string(),
                        });
                        return;
                    }
                };
                let Some(runtime) = mutation.runtime else {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: "Model mutation completed without an active runtime.".to_string(),
                    });
                    return;
                };
                app.configured_models =
                    echo_agent_app_core::model_config::configured_model_views(&mutation.config)
                        .into_iter()
                        .map(|view| {
                            echo_agent_app_core::model_config::resolve_runtime_model(
                                &mutation.config,
                                Some(&view.id),
                            )
                        })
                        .collect();
                app.model = runtime.model;
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Active model switched to: {}", app.model),
                });
            }
        }
        Some(SlashCommand::Think) => {
            if args.trim().is_empty() {
                let current = agent
                    .read(|value| {
                        value
                            .thinking()
                            .map(|config| format!("{config:?}"))
                            .unwrap_or_else(|| "model default".to_string())
                    })
                    .await;
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Thinking configuration: {current}"),
                });
            } else {
                match echo_agent::llm::ThinkingConfig::parse_spec(args.trim()) {
                    Ok(config) => {
                        agent.write(|value| value.set_thinking(config)).await;
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!("Thinking configuration set to: {}", args.trim()),
                        });
                    }
                    Err(error) => app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Invalid thinking configuration: {error}"),
                    }),
                }
            }
        }
        Some(SlashCommand::System) => {
            use echo_agent::agent::Agent;
            if args.trim().is_empty() {
                let prompt = agent.read(|value| value.current_system_prompt()).await;
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: prompt,
                });
            } else {
                agent
                    .read(|value| value.set_system_prompt(args.trim()))
                    .await;
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "System prompt updated for this runtime.".to_string(),
                });
            }
        }
        Some(SlashCommand::Memory) => {
            let layer_manager = agent
                .read(|value| value.memory_layer_manager().cloned())
                .await;
            let content = match layer_manager {
                Some(layer_manager) => {
                    let mut items = layer_manager
                        .list_hot()
                        .into_iter()
                        .map(|entry| format!("[hot] {}: {}", entry.key, entry.content))
                        .collect::<Vec<_>>();
                    match layer_manager
                        .list_warm(&echo_agent::memory::MemoryFilter::new())
                        .await
                    {
                        Ok(warm) => items.extend(
                            warm.into_iter()
                                .map(|entry| format!("[warm] {}: {}", entry.key, entry.content)),
                        ),
                        Err(error) => items.push(format!("Failed to list warm memories: {error}")),
                    }
                    if items.is_empty() {
                        "No long-term memories.".to_string()
                    } else {
                        items.join("\n")
                    }
                }
                None => "Layered memory is not configured for this agent.".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Remember) => {
            if args.trim().is_empty() {
                return push_system_message(app, "Usage: /remember <fact>".to_string());
            }
            let Some(integration) = app.review_integration.as_ref() else {
                return push_system_message(
                    app,
                    "Layered memory is not configured for this agent.".to_string(),
                );
            };
            let memory_generation = match integration.lease_generation() {
                Ok(lease) => lease,
                Err(error) => {
                    return push_system_message(
                        app,
                        format!("Cannot save memory while the workspace is switching: {error}"),
                    );
                }
            };
            let layer_manager = Arc::new(memory_generation.create_layer_manager());
            let key = uuid::Uuid::new_v4().to_string();
            let meta = echo_agent::memory::MemoryMeta::new(
                echo_agent::memory::MemoryType::ProjectFact,
                echo_agent::memory::MemorySource::ExplicitSave,
                "explicit",
            );
            let content = match layer_manager.write_memory(&key, args.trim(), meta).await {
                Ok(promotion) => {
                    if promotion.is_some() {
                        let root = agent.read(|value| value.working_dir()).await;
                        agent
                            .write_async(|value| {
                                Box::pin(async move {
                                    echo_agent_app_core::unified_memory::refresh_hot_memory_projection(
                                        value,
                                        root.as_deref(),
                                    )
                                    .await;
                                })
                            })
                            .await;
                        if let Some(pool) = &app.pool {
                            pool.refresh_hot_memory_context().await;
                        }
                    }
                    format!("Memory saved with key: {key}")
                }
                Err(error) => format!("Failed to save memory: {error}"),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Forget) => {
            if args.trim().is_empty() {
                return push_system_message(app, "Usage: /forget <key-or-query>".to_string());
            }
            let Some(integration) = app.review_integration.as_ref() else {
                return push_system_message(
                    app,
                    "Layered memory is not configured for this agent.".to_string(),
                );
            };
            let memory_generation = match integration.lease_generation() {
                Ok(lease) => lease,
                Err(error) => {
                    return push_system_message(
                        app,
                        format!("Cannot remove memory while the workspace is switching: {error}"),
                    );
                }
            };
            let layer_manager = Arc::new(memory_generation.create_layer_manager());
            let query = args.trim();
            let key = if layer_manager.locate(query).await.is_some() {
                Some(query.to_string())
            } else {
                match layer_manager.search_layered(query, 20).await {
                    Ok(matches) if matches.len() == 1 => {
                        matches.into_iter().next().map(|(_, entry)| entry.key)
                    }
                    Ok(matches) if matches.len() > 1 => {
                        let keys = matches
                            .iter()
                            .map(|(_, entry)| entry.key.chars().take(8).collect::<String>())
                            .collect::<Vec<_>>()
                            .join(", ");
                        return push_system_message(
                            app,
                            format!("Multiple memories match; use a full key or prefix: {keys}"),
                        );
                    }
                    Ok(_) => None,
                    Err(error) => {
                        return push_system_message(
                            app,
                            format!("Failed to search memory: {error}"),
                        );
                    }
                }
            };
            let content = match key {
                Some(key) => {
                    let layer = layer_manager.locate(&key).await.map(|(layer, _)| layer);
                    match layer_manager.delete_memory(&key).await {
                        Ok(true) => {
                            if layer == Some(echo_agent::evolution::MemoryLayer::Hot) {
                                let root = agent.read(|value| value.working_dir()).await;
                                agent
                                    .write_async(|value| {
                                        Box::pin(async move {
                                            echo_agent_app_core::unified_memory::refresh_hot_memory_projection(
                                                value,
                                                root.as_deref(),
                                            )
                                            .await;
                                        })
                                    })
                                    .await;
                                if let Some(pool) = &app.pool {
                                    pool.refresh_hot_memory_context().await;
                                }
                            }
                            format!("Removed memory: {key}")
                        }
                        Ok(false) => "No matching memory found.".to_string(),
                        Err(error) => format!("Failed to remove memory: {error}"),
                    }
                }
                None => "No unambiguous matching memory found.".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Clear) => {
            reset_conversation_state(app, agent, false).await;
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: "Conversation context cleared.".to_string(),
            });
        }
        Some(SlashCommand::New) => {
            reset_conversation_state(app, agent, true).await;
            if !args.trim().is_empty()
                && let (Some(store), Some(id)) = (
                    app.conversation_store.as_ref(),
                    app.conversation_id.as_deref(),
                )
            {
                let _ = store
                    .ensure_conversation(echo_agent::memory::NewConversation {
                        conversation_id: id.to_string(),
                        user_id: "default".to_string(),
                        agent_type: None,
                        title: Some(args.trim().to_string()),
                    })
                    .await;
            }
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: "New conversation started.".to_string(),
            });
        }
        Some(SlashCommand::Sessions) => {
            let Some(store) = app.conversation_store.as_ref() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Conversation persistence is unavailable.".to_string(),
                });
                return;
            };
            let result = if args.trim().is_empty() {
                store
                    .list_conversations(echo_agent::memory::ConversationFilter {
                        limit: Some(30),
                        ..Default::default()
                    })
                    .await
            } else {
                store.search_conversations(args.trim(), 30).await
            };
            let content = match result {
                Ok(items) if items.is_empty() => "No persisted conversations.".to_string(),
                Ok(items) => items
                    .into_iter()
                    .map(|item| {
                        let marker = if app.conversation_id.as_deref()
                            == Some(item.conversation_id.as_str())
                        {
                            "*"
                        } else {
                            " "
                        };
                        format!(
                            "{} {}  {:>4} messages  {}",
                            marker,
                            item.conversation_id,
                            item.message_count,
                            item.title.unwrap_or_else(|| "Untitled".to_string())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(error) => format!("Failed to list conversations: {error}"),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Resume) => {
            if app.is_processing {
                app.status_msg =
                    "Cancel the active turn before switching conversations".to_string();
            } else if args.trim().is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Usage: /resume <conversation-id>".to_string(),
                });
            } else if let Err(error) = resume_conversation(app, agent, args.trim()).await {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Failed to resume conversation: {error}"),
                });
            }
        }
        Some(SlashCommand::Fork) => {
            if app.is_processing {
                app.status_msg = "Cancel the active turn before forking".to_string();
            } else if let Err(error) = fork_conversation(app, agent, args.trim()).await {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Failed to fork conversation: {error}"),
                });
            }
        }
        Some(SlashCommand::Rename) => {
            let result = match (
                app.conversation_store.as_ref(),
                app.conversation_id.as_deref(),
            ) {
                (_, _) if args.trim().is_empty() => Err("Usage: /rename <title>".to_string()),
                (Some(store), Some(id)) => store
                    .update_conversation(id, Some(args.trim()), None, None)
                    .await
                    .map_err(|error| error.to_string()),
                _ => Err("Conversation persistence is unavailable".to_string()),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(()) => format!("Conversation renamed to: {}", args.trim()),
                    Err(error) => error,
                },
            });
        }
        Some(SlashCommand::DeleteSession) => {
            let id = args.trim();
            let result = match app.conversation_store.as_ref() {
                _ if id.is_empty() => Err("Usage: /delete-session <conversation-id>".to_string()),
                Some(store) => store
                    .delete_conversation(id)
                    .await
                    .map_err(|error| error.to_string()),
                None => Err("Conversation persistence is unavailable".to_string()),
            };
            if result.is_ok()
                && let Some(config) = agent.read(|a| a.tool_output_artifacts()).await
            {
                if let Err(error) =
                    echo_agent::tools::artifact::cleanup_tool_output_scope(&config, id, None)
                {
                    tracing::warn!(conversation_id = %id, error = %error, "Failed to clean conversation tool artifacts");
                }
                let spill_dir = config.root_dir.join("user-input");
                if let Err(error) =
                    echo_agent_app_core::prepared_turn::cleanup_user_input_scope(&spill_dir, id)
                {
                    tracing::warn!(conversation_id = %id, error = %error, "Failed to clean conversation user-input artifacts");
                }
            }
            if result.is_ok() && app.conversation_id.as_deref() == Some(id) {
                reset_conversation_state(app, agent, true).await;
            }
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(()) => format!("Deleted conversation: {id}"),
                    Err(error) => error,
                },
            });
        }
        Some(SlashCommand::OpenArtifact) => {
            let requested = args.trim();
            let from_tool = app.messages.iter().rev().find_map(|message| {
                let MessageRole::ToolExecution(tool) = &message.role else {
                    return None;
                };
                if requested.is_empty() || tool.call_id == requested {
                    tool.metadata.get("artifact_path").cloned()
                } else {
                    None
                }
            });
            let path = from_tool.or_else(|| {
                (!requested.is_empty())
                    .then(|| std::path::PathBuf::from(requested).display().to_string())
            });
            let result = match path {
                Some(path) => open_artifact_path(std::path::Path::new(&path))
                    .map(|()| format!("Opened tool-output artifact: {path}")),
                None => Err("No tool-output artifact is available".to_string()),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: result.unwrap_or_else(|error| error),
            });
        }
        Some(SlashCommand::History) => {
            let content = if app.history.is_empty() {
                "No input history in this session.".to_string()
            } else {
                app.history
                    .iter()
                    .rev()
                    .take(20)
                    .enumerate()
                    .map(|(idx, entry)| format!("{}. {}", idx + 1, entry))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
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
                    // 手动 /compact 不走 run_compact，不会发 ContextCompressed；
                    // 成功路径显式 clear_usage，与 auto-compact 效果一致。
                    app.context_snapshot.clear_usage();
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
            let enabled = match args.trim() {
                "off" => false,
                "" | "on" => true,
                _ => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: "Usage: /plan [on|off]".to_string(),
                    });
                    return;
                }
            };
            agent.write(|value| value.set_plan_mode(enabled)).await;
            app.mode = if enabled { "plan" } else { "chat" }.to_string();
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: if enabled {
                    "Entered plan mode. Write operations are disabled.".to_string()
                } else {
                    "Exited plan mode.".to_string()
                },
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
            // the file, persists it under the active workspace uploads dir, and pushes an
            // AttachmentRef onto pending_attachments. The next Enter sends it
            // alongside the typed text via drive_chat(multimodal=Some), then
            // drains the buffer. Global mode uses the shared global fallback.
            if args.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Usage: /attach <path>  (stage a file for the next message)"
                        .to_string(),
                });
            } else {
                let path = std::path::PathBuf::from(args.trim());
                match stage_attachment(
                    &mut app.pending_attachments,
                    &path,
                    app.workspace_root.as_deref(),
                ) {
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
        Some(SlashCommand::Skills) => {
            let mut parts = args.split_whitespace();
            let sub = parts.next().unwrap_or("list");
            let rest = parts.collect::<Vec<_>>().join(" ");
            let mut hub = crate::skills_hub::SkillsHub::new();
            let loaded = agent.read(|value| value.skill_names()).await;
            hub.set_loaded_skills(loaded);
            let content = match sub {
                "list" | "ls" => hub
                    .list()
                    .into_iter()
                    .map(|entry| {
                        format!(
                            "[{}] {} - {}",
                            if entry.loaded { "loaded" } else { "available" },
                            entry.name,
                            entry.description
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                "search" | "find" if !rest.is_empty() => hub
                    .search(&rest)
                    .into_iter()
                    .map(|entry| format!("{} - {}", entry.name, entry.description))
                    .collect::<Vec<_>>()
                    .join("\n"),
                "info" if !rest.is_empty() => hub.get(&rest).map_or_else(
                    || format!("Skill '{rest}' was not found."),
                    |entry| {
                        format!(
                            "{}\n{}\nPath: {}\nVersion: {}\nAuthor: {}",
                            entry.name,
                            entry.description,
                            entry.path.display(),
                            entry
                                .version
                                .clone()
                                .unwrap_or_else(|| "unknown".to_string()),
                            entry
                                .author
                                .clone()
                                .unwrap_or_else(|| "unknown".to_string())
                        )
                    },
                ),
                "refresh" => {
                    hub.refresh();
                    let root = hub.root().to_path_buf();
                    match agent
                        .write_async(|value| {
                            Box::pin(async move { value.load_skills_from_dir(root).await })
                        })
                        .await
                    {
                        Ok(names) => format!("Skills refreshed; {} loaded.", names.len()),
                        Err(error) => format!("Skill refresh failed: {error}"),
                    }
                }
                "check-updates" | "check" | "sync" => {
                    let update_args = std::iter::once(sub)
                        .chain(rest.split_whitespace())
                        .collect::<Vec<_>>();
                    crate::cli::cmd_impls::skills::execute_skill_update_command(
                        agent,
                        &update_args,
                    )
                    .await
                    .unwrap_or_else(|| "Invalid skill update command".to_string())
                }
                "install" if !rest.is_empty() => {
                    let result = if rest.starts_with("https://") || rest.ends_with(".git") {
                        crate::skills_hub::install::install_from_git(&rest, None, &mut hub).await
                    } else {
                        crate::skills_hub::install::install_from_local(
                            std::path::Path::new(&rest),
                            &mut hub,
                        )
                    };
                    match result {
                        Ok(installed) => {
                            let root = hub.root().to_path_buf();
                            let load_result = agent
                                .write_async(|value| {
                                    Box::pin(async move { value.load_skills_from_dir(root).await })
                                })
                                .await;
                            match load_result {
                                Ok(_) => format!(
                                    "Installed and loaded skill: {} ({})",
                                    installed.name,
                                    installed.path.display()
                                ),
                                Err(error) => format!(
                                    "Installed {}, but runtime reload failed: {error}",
                                    installed.name
                                ),
                            }
                        }
                        Err(error) => format!("Skill install failed: {error}"),
                    }
                }
                "uninstall" | "remove" | "rm" if !rest.is_empty() => {
                    match crate::skills_hub::install::uninstall(&rest, &mut hub) {
                        Ok(()) => {
                            format!("Uninstalled skill: {rest}. Restart to unload active content.")
                        }
                        Err(error) => format!("Skill uninstall failed: {error}"),
                    }
                }
                _ => {
                    "Usage: /skills [list|search|install|uninstall|info|refresh|check-updates|sync] [args]"
                        .to_string()
                }
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: if content.is_empty() {
                    "No matching skills.".to_string()
                } else {
                    content
                },
            });
        }
        Some(SlashCommand::Mcp) => {
            let mut parts = args.split_whitespace();
            let sub = parts.next().unwrap_or("list");
            let target = parts.collect::<Vec<_>>().join(" ");
            let content = match sub {
                "list" | "ls" => {
                    let servers = agent
                        .read(|value| {
                            value
                                .list_mcp_servers()
                                .into_iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                        })
                        .await;
                    if servers.is_empty() {
                        "No MCP servers connected.".to_string()
                    } else {
                        format!("Connected MCP servers:\n{}", servers.join("\n"))
                    }
                }
                "load" if !target.is_empty() => {
                    let path = std::path::PathBuf::from(&target);
                    match app.app_state.as_ref() {
                        Some(state) => {
                            match echo_agent_app_core::mcp_config_runtime::load_existing_mcp_config_snapshot(&path) {
                                Ok(config) => {
                                    let server_count = config.mcp_servers.len();
                                    match state
                                        .plugins
                                        .mcp_config
                                        .replace_and_reconcile(agent.clone(), config)
                                        .await
                                    {
                                        Ok(_) => format!(
                                            "Imported {server_count} MCP server(s) into the user config."
                                        ),
                                        Err(error) => format!("MCP import failed: {error}"),
                                    }
                                }
                                Err(error) => format!("MCP import failed: {error}"),
                            }
                        }
                        None => "MCP configuration runtime is unavailable.".to_string(),
                    }
                }
                "disconnect" if !target.is_empty() => match app.app_state.as_ref() {
                    Some(state) => match state
                        .plugins
                        .mcp_config
                        .remove_and_reconcile(agent.clone(), &target)
                        .await
                    {
                        Ok(_) => format!("Removed MCP server from the user config: {target}"),
                        Err(error) => format!("MCP removal failed: {error}"),
                    },
                    None => "MCP configuration runtime is unavailable.".to_string(),
                },
                _ => "Usage: /mcp [list|load <config-file>|disconnect <name>]".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Hooks) => {
            let mut parts = args.split_whitespace();
            let sub = parts.next().unwrap_or("list");
            let target = parts.next().unwrap_or("");
            let matcher = parts.next().unwrap_or("*");
            let content = match sub {
                "list" | "ls" => {
                    agent
                        .read_async(|value| {
                            Box::pin(async move {
                                let registry = value.hook_registry().read().await;
                                let sources = registry.list_sources();
                                if sources.is_empty() {
                                    "No hooks registered.".to_string()
                                } else {
                                    sources
                                        .into_iter()
                                        .map(|(name, count)| format!("{name}: {count} rule(s)"))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                }
                            })
                        })
                        .await
                }
                "reload" => {
                    // P0-1: 从磁盘重读所有 user hook 来源(含 echo-agent.yaml
                    // 内嵌),合并成单个 definition 后一次性 register。
                    let loaded = echo_agent_app_core::hook_config_loader::HookConfigLoader::load_merged_from_disk();
                    if !loaded.errors.is_empty() {
                        format!(
                            "Hook reload aborted; existing hooks are unchanged:\n{}",
                            loaded.errors.join("\n")
                        )
                    } else {
                        let count: usize = loaded.definition.rules.values().map(Vec::len).sum();
                        let definition = loaded.definition;
                        agent
                            .write_async(|value| {
                                Box::pin(async move {
                                    let mut registry = value.hook_registry().write().await;
                                    registry.clear_user_hooks();
                                    registry.register_user_hooks(definition);
                                })
                            })
                            .await;
                        format!("Hooks reloaded: {count} rule(s).")
                    }
                }
                "test" if !target.is_empty() => match parse_hook_event(target) {
                    Some(event) => {
                        let matcher = matcher.to_string();
                        let result = agent
                            .read_async(|value| {
                                Box::pin(async move {
                                    let context =
                                        echo_agent::skills::hooks::HookContext::for_dry_run(
                                            event, &matcher,
                                        );
                                    value.hook_registry().read().await.dry_run(&context)
                                })
                            })
                            .await;
                        if result.matches.is_empty() {
                            format!("Dry-run {target}: no matching actions")
                        } else {
                            result
                                .matches
                                .into_iter()
                                .map(|item| {
                                    format!(
                                        "{} · matcher={} · action={}",
                                        item.source, item.matcher, item.action
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    }
                    None => format!("Unknown hook event: {target}"),
                },
                _ => "Usage: /hooks [list|reload|test <event>]".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Plugins) => {
            let content = match app.plugin_runtime.clone() {
                Some(runtime) => handle_tui_plugin_command(app, runtime, args).await,
                None => "Plugin runtime is not initialized.".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Permission) => {
            if args.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Permission mode: {}", app.permission_mode),
                });
            } else {
                let normalized = match args.trim().to_ascii_lowercase().as_str() {
                    "ask" | "default" => "default",
                    "auto" | "auto-edit" => "auto-edit",
                    "full-auto" => "full-auto",
                    "deny" | "strict" => "strict",
                    _ => {
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: "Unknown permission mode; use default, auto-edit, full-auto, or strict".to_string(),
                        });
                        return;
                    }
                };
                agent
                    .write(|value| value.set_permission_mode(normalized))
                    .await;
                if let Some(pool) = &app.pool {
                    pool.apply_permission_mode(normalized.to_string()).await;
                }
                app.permission_mode = normalized.to_string();
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Permission mode set to: {}", app.permission_mode),
                });
            }
        }
        Some(SlashCommand::Quit) | Some(SlashCommand::Exit) => {
            app.should_quit = true;
        }
        Some(SlashCommand::AutoMemory) => {
            use echo_agent_app_core::auto_memory::{
                AutoMemoryConfig, extract_observations, format_observations_for_memory,
                queue_observations,
            };

            let sub = args.split_whitespace().next().unwrap_or("status");
            let content = match sub {
                "on" => {
                    crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    "Auto-memory enabled.".to_string()
                }
                "off" => {
                    crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    "Auto-memory disabled.".to_string()
                }
                "extract" | "show" => {
                    let evidence_generation = if sub == "extract" {
                        let Some(integration) = app.review_integration.as_ref() else {
                            return push_system_message(
                                app,
                                "Review integration is not configured.".to_string(),
                            );
                        };
                        match integration.lease_generation() {
                            Ok(lease) => Some(lease),
                            Err(error) => {
                                return push_system_message(
                                    app,
                                    format!(
                                        "Cannot queue memory candidates while the workspace is switching: {error}"
                                    ),
                                );
                            }
                        }
                    } else {
                        None
                    };
                    let messages: Vec<(String, String)> = agent
                        .read_async(|value| {
                            Box::pin(async move {
                                let context = value.context().lock().await;
                                context
                                    .messages()
                                    .iter()
                                    .map(|message| {
                                        (
                                            message.role.as_str().to_string(),
                                            message
                                                .content
                                                .as_text()
                                                .unwrap_or_default()
                                                .to_string(),
                                        )
                                    })
                                    .collect()
                            })
                        })
                        .await;
                    let observations =
                        extract_observations(&messages, &AutoMemoryConfig::default());
                    if sub == "show" {
                        if observations.is_empty() {
                            "No observations would be extracted.".to_string()
                        } else {
                            format_observations_for_memory(&observations)
                        }
                    } else {
                        let Some(evidence_generation) = evidence_generation.as_ref() else {
                            return push_system_message(
                                app,
                                "Review generation is not available.".to_string(),
                            );
                        };
                        let store = evidence_generation.evidence_store();
                        match queue_observations(&store, &observations, &messages) {
                            Ok(candidates) => format!(
                                "Queued {} auto-memory candidate(s) in Review Inbox.",
                                candidates.len()
                            ),
                            Err(error) => format!("Auto-memory extraction failed: {error}"),
                        }
                    }
                }
                "config" | "status" => {
                    let enabled = crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let config = AutoMemoryConfig::default();
                    format!(
                        "Auto-memory: {}\nMinimum confidence: {:.0}%\nMaximum per session: {}",
                        if enabled { "ON" } else { "OFF" },
                        config.min_confidence * 100.0,
                        config.max_per_session
                    )
                }
                _ => "Usage: /auto-memory <on|off|extract|show|config>".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::RunReview) => {
            let Some(review_integration) = app.review_integration.as_ref() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Review integration is not configured.".to_string(),
                });
                return;
            };
            let review_lease = match review_integration.lease_generation() {
                Ok(lease) => lease,
                Err(error) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!(
                            "Run review unavailable during workspace transition: {error}"
                        ),
                    });
                    return;
                }
            };
            let (run_store, llm_client) = agent
                .read(|value| (value.run_store.clone(), value.llm_client().cloned()))
                .await;

            let Some(run_store) = run_store else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No run store configured. Enable run tracing first.".to_string(),
                });
                return;
            };
            let Some(llm_client) = llm_client else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No LLM client available for run review.".to_string(),
                });
                return;
            };

            let runs = match run_store.list_all(1).await {
                Ok(runs) => runs,
                Err(error) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Failed to list runs: {error}"),
                    });
                    return;
                }
            };
            let Some(run_summary) = runs.first() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No runs to review.".to_string(),
                });
                return;
            };

            let reviewer = echo_agent::evolution::BackgroundReviewer::new(
                echo_agent::evolution::BackgroundReviewConfig::default(),
                llm_client,
                Some(review_lease.memory_store()),
                Some(run_store),
            );
            let reviewer =
                reviewer.with_layer_manager(Arc::new(review_lease.create_layer_manager()));

            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "Reviewing run {}...",
                    run_summary.run_id.chars().take(12).collect::<String>()
                ),
            });
            let handle = match reviewer.review_by_run_id(&run_summary.run_id) {
                Ok(handle) => handle,
                Err(error) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Run review failed: {error}"),
                    });
                    return;
                }
            };
            let settled = match review_lease.track_background_review(handle).await {
                Ok(mut pass) => pass.settle().await,
                Err(error) => Err(error),
            };
            match settled {
                Ok(settlement) if settlement.outcome.nothing_to_save => {
                    let outcome = settlement.outcome;
                    let content = outcome
                        .error
                        .map(|error| format!("Run review produced no candidate: {error}"))
                        .unwrap_or_else(|| "Run review found no durable candidate.".to_string());
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content,
                    });
                }
                Ok(settlement) => {
                    let outcome = settlement.outcome;
                    let content = match outcome.candidate {
                        Some(candidate) => format!(
                            "Candidate ({:?}, confidence {:.2}): {}\nEvidence: {}\n{}",
                            candidate.kind,
                            candidate.confidence,
                            candidate.content,
                            candidate.evidence,
                            match settlement.evidence_candidate {
                                Some(stored) => {
                                    format!("Queued in Review Inbox as {}.", stored.candidate_id)
                                }
                                None => "No inbox candidate was produced.".to_string(),
                            }
                        ),
                        None => outcome.actions.join("\n"),
                    };
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content,
                    });
                }
                Err(error) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Run review task failed: {error}"),
                    });
                }
            }
        }
        Some(SlashCommand::EvidenceInbox) => {
            use echo_agent_app_core::evolution::EvidenceReviewFilter;

            let mut parts = args.trim().splitn(3, ' ');
            let sub = parts
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or("list");
            let candidate_id = parts.next();
            let content = parts.next();
            let writes_evidence = matches!(sub, "edit" | "reject" | "accept" | "undo");
            let memory_generation = if writes_evidence {
                match app.review_integration.as_ref() {
                    Some(integration) => match integration.lease_generation() {
                        Ok(lease) => Some(lease),
                        Err(error) => {
                            return push_system_message(
                                app,
                                format!(
                                    "Cannot update Review Inbox while the workspace is switching: {error}"
                                ),
                            );
                        }
                    },
                    None => {
                        return push_system_message(
                            app,
                            "Review integration is not configured.".to_string(),
                        );
                    }
                }
            } else {
                None
            };
            let store = memory_generation
                .as_ref()
                .map(|lease| lease.evidence_store())
                .or_else(|| {
                    app.review_integration
                        .as_ref()
                        .map(|integration| integration.evidence_store())
                })
                .unwrap_or_else(|| {
                    echo_agent_app_core::evolution::EvidenceStore::new(
                        echo_agent_app_core::evolution::discover_echo_agent_dir(),
                    )
                });
            let result = match sub {
                "list" | "ls" | "pending" | "expired" | "stale" | "applied"
                | "undoable" => {
                    let filter = match sub {
                        "expired" | "stale" => EvidenceReviewFilter::Expired,
                        "applied" | "undoable" => EvidenceReviewFilter::Undoable,
                        _ => EvidenceReviewFilter::Pending,
                    };
                    match store.review_items() {
                        Ok(candidates) => {
                            let lines: Vec<_> = candidates
                                .into_iter()
                                .filter(|candidate| filter.matches(candidate))
                                .map(|item| {
                                    let candidate = item.candidate;
                                    let state = if item.expired {
                                        "Expired"
                                    } else if matches!(
                                        candidate.status,
                                        echo_agent_app_core::evolution::EvidenceCandidateStatus::Applied
                                    ) {
                                        "Undoable"
                                    } else {
                                        "Ready"
                                    };
                                    format!(
                                        "{} [{} / {:?}] {:.2} {}",
                                        candidate.candidate_id,
                                        state,
                                        candidate.kind,
                                        candidate.confidence,
                                        candidate.content
                                    )
                                })
                                .collect();
                            if lines.is_empty() {
                                "Review Inbox is empty for this filter.".to_string()
                            } else {
                                lines.join("\n")
                            }
                        }
                        Err(error) => format!("Failed to read Review Inbox: {error}"),
                    }
                }
                "show" => match candidate_id {
                    Some(id) => match store.review_item(id) {
                        Ok(Some(item)) => {
                            let candidate = item.candidate;
                            let evidence = candidate
                                .evidence
                                .iter()
                                .map(|item| {
                                    format!(
                                        "[{:?}/{}] {}",
                                        item.source,
                                        item.source_role.as_deref().unwrap_or("unknown"),
                                        item.quote
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            format!(
                                "{}\nKind: {:?}  Status: {:?}{}  Confidence: {:.2}\n{}",
                                candidate.content,
                                candidate.kind,
                                candidate.status,
                                if item.expired { " (expired)" } else { "" },
                                candidate.confidence,
                                evidence
                            )
                        }
                        Ok(None) => format!("Candidate '{id}' not found."),
                        Err(error) => format!("Failed to read candidate: {error}"),
                    },
                    None => "Usage: /evidence-inbox show <candidate-id>".to_string(),
                },
                "edit" => match (candidate_id, content) {
                    (Some(id), Some(new_content)) => match store.edit(id, new_content) {
                        Ok(candidate) => format!("Updated {}.", candidate.candidate_id),
                        Err(error) => format!("Failed to edit candidate: {error}"),
                    },
                    _ => {
                        "Usage: /evidence-inbox edit <candidate-id> <new-content>".to_string()
                    }
                },
                "reject" => match candidate_id {
                    Some(id) => match store.reject(id) {
                        Ok(candidate) => format!("Rejected {}.", candidate.candidate_id),
                        Err(error) => format!("Failed to reject candidate: {error}"),
                    },
                    None => "Usage: /evidence-inbox reject <candidate-id>".to_string(),
                },
                "accept" | "undo" => match candidate_id {
                    Some(id) => match memory_generation.as_ref() {
                        Some(lease) => {
                            let layer_manager = Arc::new(lease.create_layer_manager());
                            let action = if sub == "accept" {
                                store.accept(id, content, &layer_manager).await
                            } else {
                                store.undo(id, &layer_manager).await
                            };
                            match action {
                                Ok(candidate) => {
                                    format!("{} is now {:?}.", candidate.candidate_id, candidate.status)
                                }
                                Err(error) => format!("Review Inbox action failed: {error}"),
                            }
                        }
                        None => "Review generation is not available.".to_string(),
                    },
                    None => format!("Usage: /evidence-inbox {sub} <candidate-id>"),
                },
                _ => "Usage: /evidence-inbox <pending|expired|undoable|show|edit|accept|reject|undo> [candidate-id] [content]".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: result,
            });
        }
        Some(SlashCommand::EvolutionDashboard) => {
            let (store, run_store) = agent
                .read(|value| (value.store().cloned(), value.run_store.clone()))
                .await;
            let Some(store) = store else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No memory store configured.".to_string(),
                });
                return;
            };
            let echo_agent_dir = app
                .review_integration
                .as_ref()
                .map(|integration| integration.echo_agent_dir())
                .unwrap_or_else(echo_agent_app_core::evolution::discover_echo_agent_dir);
            let change_log = echo_agent::evolution::JsonlChangeLog::new(
                echo_agent_dir.join("evolution").join("change-log.jsonl"),
            );
            let dashboard = echo_agent_app_core::evolution::Dashboard::new(store, change_log)
                .with_run_store(run_store);
            let metrics = dashboard.generate_metrics().await;
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: echo_agent_app_core::evolution::Dashboard::format_metrics(&metrics),
            });
        }
        Some(SlashCommand::MemoryReview) => match app.review_integration.as_ref() {
            Some(review_integration) => {
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
                    content: "Memory review integration is not configured for this agent."
                        .to_string(),
                });
            }
        },
        Some(SlashCommand::SkillCandidates) => {
            // List candidates and drafts from Curator state
            let curator = app
                .review_integration
                .as_ref()
                .map(|integration| integration.curator())
                .unwrap_or_else(|| {
                    echo_agent_app_core::evolution::workspace_curator(
                        &echo_agent_app_core::evolution::discover_echo_agent_dir(),
                    )
                });
            let state = match curator.load_state() {
                Ok(state) => state,
                Err(error) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Curator state unavailable: {error}"),
                    });
                    return;
                }
            };
            let items: Vec<_> = state
                .skills
                .iter()
                .filter(|(_, m)| {
                    matches!(
                        m.lifecycle,
                        echo_agent::evolution::SkillLifecycle::Candidate
                            | echo_agent::evolution::SkillLifecycle::Draft
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
                        echo_agent::evolution::SkillLifecycle::Candidate => "🎯",
                        echo_agent::evolution::SkillLifecycle::Draft => "📝",
                        _ => "  ",
                    };
                    content.push_str(&format!("  {} {} [{:?}]\n", icon, name, meta.lifecycle));
                }
                content.push_str("\nUse /skills to inspect, install, refresh, or remove skills.");
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
        Some(SlashCommand::Workspace) => {
            let command_args = args.split_whitespace().collect::<Vec<_>>();
            let Some(app_state) = app.app_state.clone() else {
                push_system_message(
                    app,
                    "Workspace management is unavailable in this runtime.".to_string(),
                );
                return;
            };
            let result = crate::cli::cmd_impls::workspace::execute_workspace_command(
                Some(&app_state),
                &command_args,
            )
            .await;
            if result.generation_changed {
                refresh_workspace_generation(app, agent, app_state.as_ref()).await;
            }
            push_system_message(app, result.output);
        }
        Some(SlashCommand::Cost) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "Token usage: prompt={}, completion={}, requests={}",
                    app.tokens.0, app.tokens.1, app.tokens.2
                ),
            });
        }
        Some(SlashCommand::Analysis) => {
            let command_args: Vec<&str> = args.split_whitespace().collect();
            let content =
                crate::cli::cmd_impls::analysis::execute_analysis_command(agent, &command_args)
                    .await;
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Trace) => {
            let run_store = agent.read(|value| value.run_store().cloned()).await;
            let content = match run_store {
                None => "Run store not configured.".to_string(),
                Some(store) => {
                    let diagnostic_id = if args.trim().is_empty() {
                        match echo_agent_app_core::observability::list_diagnostic_runs(
                            store.as_ref(),
                        )
                        .await
                        {
                            Ok(runs) => Ok(runs.first().map(|run| run.diagnostic_id.clone())),
                            Err(error) => Err(format!("Unable to list run diagnostics: {error}")),
                        }
                    } else {
                        Ok(Some(args.trim().to_string()))
                    };
                    match diagnostic_id {
                        Err(message) => message,
                        Ok(None) => "No durable run diagnostics available.".to_string(),
                        Ok(Some(diagnostic_id)) => {
                            match echo_agent_app_core::observability::load_run_diagnostics(
                                store.as_ref(),
                                &diagnostic_id,
                                app.prompt_assembly.clone(),
                            )
                            .await
                            {
                                Ok(Some(diagnostics)) => {
                                    echo_agent_app_core::observability::format_run_diagnostics(
                                        &diagnostics,
                                    )
                                }
                                Ok(None) => {
                                    format!("Run diagnostics not found: {diagnostic_id}")
                                }
                                Err(error) => {
                                    format!("Unable to load run diagnostics: {error}")
                                }
                            }
                        }
                    }
                }
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::PromptDiagnostics) => {
            let context = agent.read(|value| value.context().clone()).await;
            let (message_count, estimated_tokens, protected_message_count, protected_tokens) = {
                let context = context.lock().await;
                (
                    context.messages().len(),
                    context.token_estimate(),
                    context.protected_message_count(),
                    context.protected_token_estimate(),
                )
            };
            let mut content = String::from("Prompt diagnostics (local estimates):\n");
            if let Some(assembly) = app.prompt_assembly.as_ref() {
                content.push_str(&format!(
                    "  Static prompt: ~{} tokens\n",
                    assembly.estimated_tokens
                ));
                for module in &assembly.modules {
                    let status = if !module.included {
                        "omitted"
                    } else if module.truncated {
                        "truncated"
                    } else {
                        "full"
                    };
                    content.push_str(&format!(
                        "    {:<24} ~{:>6} tokens  {}\n",
                        module.name, module.estimated_tokens, status
                    ));
                }
            } else {
                content.push_str("  Static prompt report: unavailable\n");
            }
            content.push_str(&format!(
                "  Current context: ~{} tokens across {} messages\n",
                estimated_tokens, message_count
            ));
            content.push_str(&format!(
                "  Protected context: ~{} tokens across {} messages",
                protected_tokens, protected_message_count
            ));
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Tools) => {
            let tools = agent.read(|value| value.tool_names()).await;
            app.tool_count = tools.len();
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: if tools.is_empty() {
                    "No tools registered.".to_string()
                } else {
                    format!("Available tools ({}):\n{}", tools.len(), tools.join("\n"))
                },
            });
        }
        Some(SlashCommand::Tasks) => {
            app.sidebar_visible = true;
            app.sidebar_tab = 2;
            refresh_task_runtime_view(app);
            let mut content = app
                .task_runtime_view
                .as_ref()
                .map(format_task_runtime_view)
                .unwrap_or_else(|| "No TaskRuntime run for this conversation.".to_string());
            append_subagent_summary(&mut content, &app.subagent_runs);
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Steer) => {
            let instruction = args.trim();
            if instruction.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Usage: /steer <instruction>".to_string(),
                });
                return;
            }
            let attachments = std::mem::take(&mut app.pending_attachments);
            let snapshot = match active_tui_turn(app) {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    queue_steer_follow_up(app, instruction, attachments);
                    return;
                }
            };
            let spill_dir = echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(
                app.workspace_root.as_deref(),
            );
            let prepared = match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
                echo_agent_app_core::prepared_turn::UserTurnInput {
                    text: instruction,
                    attachments: &attachments,
                    spill_dir: &spill_dir,
                    conversation_id: app.conversation_id.as_deref(),
                    turn_id: Some(&snapshot.turn_id),
                },
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Failed to prepare steer input: {error}"),
                    });
                    app.pending_attachments = attachments;
                    return;
                }
            };
            let message = match prepared.to_message() {
                Ok(message) => message,
                Err(error) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Failed to build steer message: {error}"),
                    });
                    app.pending_attachments = attachments;
                    return;
                }
            };
            match agent.steer_input(Some(&snapshot.turn_id), message).await {
                Ok(turn_id) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::User,
                        content: instruction.to_string(),
                    });
                    app.status_msg = format!("Guidance injected into turn {turn_id}");
                }
                Err(
                    echo_agent::agent::TurnSteerError::NoActiveTurn
                    | echo_agent::agent::TurnSteerError::NotSteerable { .. }
                    | echo_agent::agent::TurnSteerError::TurnMismatch { .. },
                ) => {
                    queue_steer_follow_up(app, instruction, attachments);
                }
                Err(error) => {
                    app.pending_attachments = attachments;
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Steer failed: {error}"),
                    });
                }
            }
        }
        Some(SlashCommand::TaskCancel)
        | Some(SlashCommand::TaskPause)
        | Some(SlashCommand::TaskResume) => {
            let Some(action) = slash_cmd else {
                return;
            };
            let Some(store) = app.task_runtime_store.as_ref().cloned() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Task runtime is unavailable.".to_string(),
                });
                return;
            };
            let run_id = if args.trim().is_empty() {
                app.task_runtime_view
                    .as_ref()
                    .map(|view| view.run_id.clone())
            } else {
                Some(args.trim().to_string())
            };
            let Some(run_id) = run_id else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No active task run. Supply a run id explicitly.".to_string(),
                });
                return;
            };
            let result = match action {
                SlashCommand::TaskCancel => store
                    .request_cancel(&run_id)
                    .map_err(|error| error.to_string())
                    .and_then(|cancelled| {
                        cancelled
                            .then_some("cancelled")
                            .ok_or_else(|| "run is not cancellable".to_string())
                    }),
                SlashCommand::TaskPause => store
                    .request_pause(&run_id)
                    .map_err(|error| error.to_string())
                    .and_then(|paused| {
                        paused
                            .then_some("paused")
                            .ok_or_else(|| "run is not actively pausable".to_string())
                    }),
                SlashCommand::TaskResume => {
                    let continuation_run = store
                        .get_run_state(&run_id)
                        .ok()
                        .flatten()
                        .filter(|state| {
                            state
                                .continuation
                                .as_ref()
                                .is_some_and(|continuation| continuation.enabled)
                        })
                        .map(|state| state.run);
                    if let Some(continuation_run) = continuation_run {
                        match dispatch_turn(
                            app,
                            agent,
                            agent_tx.clone(),
                            QueuedTurn {
                                text: format!(
                                    "Continue the existing TaskRun {run_id} toward its unchanged Goal."
                                ),
                                attachments: Vec::new(),
                                interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode::Task,
                                run_resume: Some(crate::tui::QueuedRunResume {
                                    run_id: continuation_run.run_id,
                                    root_message_id: continuation_run.root_message_id,
                                }),
                            },
                        )
                        .await
                        {
                            TurnDispatchResult::Started => Ok("continuation submitted"),
                            TurnDispatchResult::Rejected { error, .. } => Err(error),
                        }
                    } else {
                        resume_tui_task_run(
                            store.clone(),
                            agent.clone(),
                            run_id.clone(),
                            app.review_integration.clone(),
                        )
                        .await
                    }
                }
                _ => Err("unsupported task action".to_string()),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(label) => format!("Task run {run_id} {label}."),
                    Err(error) => format!("Task run action failed: {error}"),
                },
            });
            refresh_task_runtime_view(app);
        }
        Some(SlashCommand::TaskBudget) => {
            let Some(store) = app.task_runtime_store.as_ref().cloned() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Task runtime is unavailable.".to_string(),
                });
                return;
            };
            let mut values = args.split_whitespace();
            let Some(token_value) = values.next() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Usage: /task-budget <tokens|none> <seconds|none> [run-id]"
                        .to_string(),
                });
                return;
            };
            let Some(time_value) = values.next() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Usage: /task-budget <tokens|none> <seconds|none> [run-id]"
                        .to_string(),
                });
                return;
            };
            let run_id = values.next().map(str::to_string).or_else(|| {
                app.task_runtime_view
                    .as_ref()
                    .map(|view| view.run_id.clone())
            });
            let result = run_id
                .ok_or_else(|| "No active task run. Supply a run id explicitly.".to_string())
                .and_then(|run_id| {
                    parse_tui_budget(token_value, "token").and_then(|tokens| {
                        parse_tui_budget(time_value, "time").and_then(|time| {
                            store
                                .update_run_continuation_budgets(&run_id, tokens, time)
                                .map(|_| run_id)
                                .map_err(|error| error.to_string())
                        })
                    })
                });
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(run_id) => format!("Task run {run_id} budgets updated."),
                    Err(error) => format!("Task budget update failed: {error}"),
                },
            });
            refresh_task_runtime_view(app);
        }
        Some(SlashCommand::TaskGoal) => {
            let Some(store) = app.task_runtime_store.as_ref().cloned() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Task runtime is unavailable.".to_string(),
                });
                return;
            };
            let values = args.split_whitespace().collect::<Vec<_>>();
            let result =
                crate::task_run_control::parse_run_goal_update_args(&values).and_then(|parsed| {
                    let run_id = parsed.requested_run_id.or_else(|| {
                        app.task_runtime_view
                            .as_ref()
                            .map(|view| view.run_id.clone())
                    });
                    let run_id = run_id.ok_or_else(|| {
                        "No active task run. Supply a run id explicitly.".to_string()
                    })?;
                    store
                        .update_run_goal(
                            &run_id,
                            parsed.expected_goal_revision,
                            &parsed.new_goal,
                            &parsed.reason,
                            echo_agent_app_core::tasks::task_runtime::RunGoalActorSource::Tui,
                        )
                        .map_err(|error| error.to_string())
                });
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(run) => format!(
                        "Task run {} Goal updated to revision {}; update its task graph before resuming.",
                        run.run_id, run.goal_revision
                    ),
                    Err(error) => format!("Task Goal update failed: {error}"),
                },
            });
            refresh_task_runtime_view(app);
        }
        Some(SlashCommand::TaskRecovery) => {
            let Some(store) = app.task_runtime_store.as_ref() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Task runtime is unavailable.".to_string(),
                });
                return;
            };
            let run_id = if args.trim().is_empty() {
                app.task_runtime_view
                    .as_ref()
                    .map(|view| view.run_id.clone())
            } else {
                Some(args.trim().to_string())
            };
            let Some(run_id) = run_id else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No active task run. Supply a run id explicitly.".to_string(),
                });
                return;
            };
            let content = match store.list_recovery_blockers(&run_id) {
                Ok(blockers) if blockers.is_empty() => {
                    format!("Task run {run_id} has no recovery blockers.")
                }
                Ok(blockers) => {
                    let details = blockers
                        .iter()
                        .map(|blocker| format!("{}: {}", blocker.task_id, blocker.reason))
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!(
                        "Recovery blockers for {run_id}:\n{details}\nUse /task-retry <task-id> or /task-skip <task-id>."
                    )
                }
                Err(error) => format!("Failed to read recovery blockers: {error}"),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::TaskRetry) | Some(SlashCommand::TaskSkip) => {
            let Some(action) = slash_cmd else {
                return;
            };
            let Some(store) = app.task_runtime_store.as_ref() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Task runtime is unavailable.".to_string(),
                });
                return;
            };
            let mut parts = args.split_whitespace();
            let Some(task_id) = parts.next() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Usage: {} <task-id> [run-id]", action.slash_name()),
                });
                return;
            };
            let run_id = parts.next().map(str::to_string).or_else(|| {
                app.task_runtime_view
                    .as_ref()
                    .map(|view| view.run_id.clone())
            });
            let Some(run_id) = run_id else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No active task run. Supply a run id explicitly.".to_string(),
                });
                return;
            };
            let result = if action == SlashCommand::TaskRetry {
                retry_tui_task(
                    store.clone(),
                    agent.clone(),
                    run_id.clone(),
                    task_id.to_string(),
                    app.review_integration.clone(),
                )
                .await
                .map_err(|error| error.to_string())
            } else {
                store
                    .resolve_recovery_task(
                        &run_id,
                        task_id,
                        echo_agent_app_core::tasks::task_runtime::RecoveryDecision::Skip,
                    )
                    .map(|()| format!("Recovery decision recorded for {run_id}/{task_id}: skip."))
                    .map_err(|e| e.to_string())
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(msg) => msg,
                    Err(error) => format!("Failed to retry/skip task: {error}"),
                },
            });
            refresh_task_runtime_view(app);
        }
        Some(SlashCommand::Preview) => {
            match resolve_tui_workspace_file(args) {
                Ok(path) => match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        let limit = 40_000;
                        let truncated = content.chars().count() > limit;
                        let preview = content.chars().take(limit).collect::<String>();
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!(
                                "--- {}{} ---\n{}",
                                path.display(),
                                if truncated { " (truncated)" } else { "" },
                                preview
                            ),
                        });
                    }
                    Err(error) => app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Preview failed: {error}"),
                    }),
                },
                Err(error) => app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Preview failed: {error}"),
                }),
            }
            app.rebuild_message_groups();
        }
        Some(SlashCommand::Edit) => match resolve_tui_workspace_file(args) {
            Ok(path) => {
                app.external_file_editor_requested = Some(path);
                app.status_msg = "Opening file editor...".to_string();
            }
            Err(error) => {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Edit failed: {error}"),
                });
                app.rebuild_message_groups();
            }
        },
        Some(SlashCommand::Browser) => {
            let Some(runtime) = app.browser_runtime.clone() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Browser runtime is unavailable.".to_string(),
                });
                app.rebuild_message_groups();
                return;
            };
            let requested = args.trim().to_ascii_lowercase();
            if requested.is_empty() || requested == "status" {
                let status = runtime.extension_status().await;
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!(
                        "Playwright Extension: {}\nPackage: {}\nConnection token: {}{}",
                        if status.connected {
                            "connected"
                        } else {
                            "disconnected"
                        },
                        status.package,
                        if status.token_configured {
                            "configured"
                        } else {
                            "not configured"
                        },
                        status
                            .startup_error
                            .map(|error| format!("\nError: {error}"))
                            .unwrap_or_default()
                    ),
                });
            } else if requested == "managed" || requested == "chrome" {
                let conversation_id = app
                    .conversation_id
                    .clone()
                    .unwrap_or_else(|| "tui-preview".to_string());
                let params = std::collections::HashMap::from([(
                    "backend".to_string(),
                    serde_json::Value::String(requested.clone()),
                )]);
                let result = runtime
                    .execute_main(
                        conversation_id,
                        echo_agent_app_core::browser::BrowserAction::Backend,
                        params,
                        None,
                    )
                    .await;
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: match result {
                        Ok(_) => format!("Browser backend switched to {requested}."),
                        Err(error) => format!("Browser backend switch failed: {error}"),
                    },
                });
            } else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Usage: /browser [status|managed|chrome]".to_string(),
                });
            }
            app.rebuild_message_groups();
        }
        Some(SlashCommand::Worktrees) => {
            let content = handle_tui_worktrees(app, args).await;
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
            app.rebuild_message_groups();
        }
        Some(SlashCommand::Cron) => {
            let content = handle_tui_cron(app, args).await;
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
            app.rebuild_message_groups();
        }
        Some(SlashCommand::Test)
        | Some(SlashCommand::CodeReview)
        | Some(SlashCommand::Diff)
        | Some(SlashCommand::Git)
        | Some(SlashCommand::Pipeline) => {
            let prompt = match slash_cmd.unwrap_or(SlashCommand::Status) {
                SlashCommand::Test => format!(
                    "Run the relevant project tests{} and report failures with actionable fixes.",
                    if args.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" matching '{}'", args.trim())
                    }
                ),
                SlashCommand::CodeReview => format!(
                    "Review {}. Prioritize bugs, regressions, security issues, and missing tests; provide file and line references.",
                    if args.trim().is_empty() {
                        "the current changes"
                    } else {
                        args.trim()
                    }
                ),
                SlashCommand::Diff => format!(
                    "Inspect and explain the current diff{}.",
                    if args.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" for {}", args.trim())
                    }
                ),
                SlashCommand::Git => format!(
                    "Perform this git operation and report the result: {}",
                    args.trim()
                ),
                SlashCommand::Pipeline => {
                    format!("Manage the requested pipeline operation: {}", args.trim())
                }
                _ => String::new(),
            };
            let result = dispatch_turn(
                app,
                agent,
                agent_tx,
                QueuedTurn {
                    text: prompt,
                    attachments: Vec::new(),
                    interaction_mode: app.interaction_mode,
                    run_resume: None,
                },
            )
            .await;
            project_queued_dispatch(app, result);
        }
        None => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Unknown command: {command}"),
            });
        }
    }

    if !app.is_processing {
        app.status_msg = "Ready".to_string();
    }
}

async fn resume_tui_task_run(
    store: Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
    agent: AgentHandle,
    run_id: String,
    review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
) -> Result<&'static str, String> {
    let preparation_store = store.clone();
    let preparation_run_id = run_id.clone();
    start_tui_task_run_driver(store, agent, run_id, review_integration, move || {
        preparation_store.resume_task_run(&preparation_run_id)
    })
    .await
    .map_err(|error| error.to_string())?;
    Ok("resumed")
}

async fn retry_tui_task(
    store: Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
    agent: AgentHandle,
    run_id: String,
    task_id: String,
    review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
) -> Result<String, echo_agent_app_core::tasks::task_runtime::StoreError> {
    let preparation = start_tui_task_retry_driver(
        store,
        agent,
        run_id.clone(),
        task_id.clone(),
        review_integration,
    )
    .await?;
    match preparation {
        echo_agent_app_core::tasks::task_runtime::TaskRetryPreparation::Acceptance {
            next_attempt,
        } => Ok(format!(
            "Task {task_id} retried as attempt {next_attempt} on run {run_id}; executor started."
        )),
        echo_agent_app_core::tasks::task_runtime::TaskRetryPreparation::Recovery => Ok(format!(
            "Recovery decision recorded for {run_id}/{task_id}: retry."
        )),
    }
}

async fn start_tui_task_retry_driver(
    store: Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
    agent: AgentHandle,
    run_id: String,
    task_id: String,
    review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
) -> Result<
    echo_agent_app_core::tasks::task_runtime::TaskRetryPreparation,
    echo_agent_app_core::tasks::task_runtime::StoreError,
> {
    let cancel = echo_agent::agent::CancellationToken::new();
    let preparation_store = store.clone();
    let preparation_run_id = run_id.clone();
    let (preparation, _) = store.spawn_supervised_task_retry(
        run_id,
        task_id,
        cancel.clone(),
        move || {
            let memory_generation = review_integration
                .as_ref()
                .map(|integration| integration.lease_generation())
                .transpose()
                .map_err(|error| {
                    echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(format!(
                        "memory generation unavailable: {error}"
                    ))
                })?;
            let layer_manager = memory_generation
                .as_ref()
                .map(|generation| Arc::new(generation.create_layer_manager()));
            Ok((memory_generation, layer_manager))
        },
        move |(memory_generation, layer_manager), mut receipt_owner| async move {
            if let Some(generation) = memory_generation.as_ref() {
                receipt_owner.retain(generation.clone());
            }
            let reviewer_llm = agent.read(|value| value.llm_client().cloned()).await;
            let run_store = agent.read(|value| value.run_store().cloned()).await;
            let result = echo_agent_app_core::tasks::task_runtime::execute_run(
                preparation_store,
                Some(agent),
                reviewer_llm,
                layer_manager,
                memory_generation,
                run_store,
                None,
                &preparation_run_id,
                cancel,
                echo_agent_app_core::tasks::task_runtime::MemoryPolicy::BestEffortSettled,
            )
            .await;
            if let Err(error) = result {
                tracing::error!(run_id = %preparation_run_id, %error, "TUI task retry driver failed");
                return Err(error.to_string());
            }
            Ok(())
        },
    )?;
    Ok(preparation)
}

async fn start_tui_task_run_driver<Prepared, Prepare>(
    store: Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
    agent: AgentHandle,
    run_id: String,
    review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    prepare: Prepare,
) -> Result<Prepared, echo_agent_app_core::tasks::task_runtime::StoreError>
where
    Prepare: FnOnce() -> Result<Prepared, echo_agent_app_core::tasks::task_runtime::StoreError>,
{
    let cancel = echo_agent::agent::CancellationToken::new();
    let supervisor_cancel = cancel.clone();
    let validation_store = store.clone();
    let validation_run_id = run_id.clone();
    let preparation_store = store.clone();
    let preparation_run_id = run_id.clone();
    let (prepared, _) = store
        .spawn_supervised_run_driver(run_id, supervisor_cancel, move || {
            let memory_generation = review_integration
                .as_ref()
                .map(|integration| integration.lease_generation())
                .transpose()
                .map_err(|error| {
                    echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(format!(
                        "memory generation unavailable: {error}"
                    ))
                })?;
            let layer_manager = memory_generation
                .as_ref()
                .map(|generation| Arc::new(generation.create_layer_manager()));
            if validation_store.get_plan(&validation_run_id)?.is_none() {
                return Err(
                    echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(
                        "run has no persisted plan to resume".to_string(),
                    ),
                );
            }
            Ok((memory_generation, layer_manager))
        }, move |(memory_generation, layer_manager)| {
            let prepared = prepare()?;
            Ok((prepared, move |mut receipt_owner: echo_agent_app_core::tasks::task_runtime::RunDriverReceiptOwner| async move {
                if let Some(generation) = memory_generation.as_ref() {
                    receipt_owner.retain(generation.clone());
                }
                let reviewer_llm = agent.read(|value| value.llm_client().cloned()).await;
                let run_store = agent.read(|value| value.run_store().cloned()).await;
                let result = echo_agent_app_core::tasks::task_runtime::execute_run(
                    preparation_store,
                    Some(agent),
                    reviewer_llm,
                    layer_manager,
                    memory_generation,
                    run_store,
                    None,
                    &preparation_run_id,
                    cancel,
                    echo_agent_app_core::tasks::task_runtime::MemoryPolicy::BestEffortSettled,
                )
                .await;
                if let Err(error) = result {
                    tracing::error!(run_id = %preparation_run_id, %error, "TUI task run driver failed");
                    return Err(error.to_string());
                }
                Ok(())
            }))
        })?;
    Ok(prepared)
}

fn resolve_tui_workspace_file(value: &str) -> anyhow::Result<std::path::PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("a file path is required"));
    }
    let root = std::env::current_dir()?.canonicalize()?;
    let requested = std::path::PathBuf::from(trimmed);
    let target = if requested.is_absolute() {
        requested
    } else {
        root.join(requested)
    }
    .canonicalize()?;
    if !target.starts_with(&root) {
        return Err(anyhow::anyhow!("file is outside the current workspace"));
    }
    if !target.is_file() {
        return Err(anyhow::anyhow!("path is not a file"));
    }
    Ok(target)
}

async fn reset_conversation_state(app: &mut TuiApp, agent: &AgentHandle, new_id: bool) {
    if !new_id
        && let (Some(store), Some(id)) = (
            app.conversation_store.as_ref(),
            app.conversation_id.as_deref(),
        )
        && let Err(error) = store.save_messages(id, &[]).await
    {
        tracing::warn!(error = %error, conversation_id = id, "failed to clear persisted conversation");
    }
    if new_id {
        let id = uuid::Uuid::new_v4().to_string();
        app.conversation_id = Some(id.clone());
        agent.write(|value| value.set_conversation_id(id)).await;
    }
    app.messages.clear();
    app.tokens = (0, 0, 0);
    app.streaming_text.clear();
    app.pending_stream.clear();
    app.pending_attachments.clear();
    app.queued_turns.clear();
    app.active_turn_id = None;
    app.task_runtime_view = None;
    app.subagent_runs.clear();
    app.is_processing = false;
    app.chat_scroll = 0;
    app.clear_selection();
    app.context_snapshot.clear_usage();
    app.usage_accumulator.reset();
    agent
        .read_async(|value| {
            Box::pin(async move {
                use echo_agent::agent::Agent;
                value.reset().await;
            })
        })
        .await;
}

async fn resume_conversation(
    app: &mut TuiApp,
    agent: &AgentHandle,
    conversation_id: &str,
) -> anyhow::Result<()> {
    let store = app
        .conversation_store
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("conversation persistence is unavailable"))?;
    let conversation = store
        .get_conversation(conversation_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("conversation '{conversation_id}' was not found"))?;
    let stored = store.get_messages(conversation_id).await?;
    let runtime_messages = match echo_agent::memory::restore_messages(&stored) {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::warn!(error = %e, "failed to restore messages; continuing with empty history");
            Vec::new()
        }
    };
    agent
        .write(|value| value.set_conversation_id(conversation_id.to_string()))
        .await;
    agent
        .read_async(|value| Box::pin(async move { value.load_messages(runtime_messages).await }))
        .await;

    app.conversation_id = Some(conversation_id.to_string());
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
        content: format!(
            "Resumed conversation: {} ({})",
            conversation.title.unwrap_or_else(|| "Untitled".to_string()),
            conversation_id
        ),
    });
    app.task_runtime_view = None;
    app.subagent_runs.clear();
    app.chat_scroll = 0;
    app.rebuild_message_groups();
    Ok(())
}

async fn fork_conversation(
    app: &mut TuiApp,
    agent: &AgentHandle,
    title: &str,
) -> anyhow::Result<()> {
    let store = app
        .conversation_store
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("conversation persistence is unavailable"))?;
    let id = uuid::Uuid::new_v4().to_string();
    let runtime_messages = agent
        .read_async(|value| Box::pin(async move { value.get_messages().await }))
        .await;
    let projected = echo_agent::memory::project_messages(&id, &runtime_messages)?;
    let default_title = app
        .conversation_id
        .as_deref()
        .map(|source| format!("Fork of {}", source.chars().take(8).collect::<String>()))
        .unwrap_or_else(|| "Forked conversation".to_string());
    store
        .create_conversation(echo_agent::memory::NewConversation {
            conversation_id: id.clone(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some(if title.is_empty() {
                default_title
            } else {
                title.to_string()
            }),
        })
        .await?;
    store.save_messages(&id, &projected).await?;
    agent
        .write(|value| value.set_conversation_id(id.clone()))
        .await;
    app.conversation_id = Some(id.clone());
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        content: format!("Forked into conversation: {id}"),
    });
    Ok(())
}

fn refresh_task_runtime_view(app: &mut TuiApp) {
    let Some(store) = &app.task_runtime_store else {
        app.task_runtime_view = None;
        return;
    };
    let Some(conversation_id) = app.conversation_id.as_deref() else {
        app.task_runtime_view = None;
        return;
    };
    let run = match store.latest_run_for_conversation(conversation_id) {
        Ok(run) => run,
        Err(e) => {
            tracing::warn!(error = %e, "TUI failed to refresh TaskRuntime run");
            return;
        }
    };
    let Some(run) = run else {
        app.task_runtime_view = None;
        return;
    };
    let tasks = match store.get_plan(&run.run_id) {
        Ok(Some(plan)) => plan
            .tasks
            .into_iter()
            .map(|task| TaskRuntimeTaskView {
                title: task.title,
                status: task.status.as_str().to_string(),
                agent_role: task.agent_role,
            })
            .collect(),
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!(error = %e, run_id = %run.run_id, "TUI failed to refresh TaskRuntime plan");
            Vec::new()
        }
    };
    let continuation = store
        .get_run_state(&run.run_id)
        .ok()
        .flatten()
        .and_then(|state| state.continuation);
    let active_cell_count = store
        .list_background_cells(&run.run_id)
        .map(|cells| cells.iter().filter(|cell| cell.is_active()).count())
        .unwrap_or(0);
    app.task_runtime_view = Some(TaskRuntimeView {
        run_id: run.run_id,
        goal: run.goal,
        goal_revision: run.goal_revision,
        status: run.status.as_str().to_string(),
        continuation_enabled: continuation.as_ref().is_some_and(|state| state.enabled),
        turn_ordinal: continuation
            .as_ref()
            .and_then(|state| state.active_turn.as_ref().or(state.last_turn.as_ref()))
            .map(|turn| turn.ordinal),
        tokens_used: continuation
            .as_ref()
            .map(|state| state.tokens_used)
            .unwrap_or(0),
        token_budget: continuation.as_ref().and_then(|state| state.token_budget),
        time_used_seconds: continuation
            .as_ref()
            .map(|state| state.time_used_seconds)
            .unwrap_or(0),
        time_budget_seconds: continuation
            .as_ref()
            .and_then(|state| state.time_budget_seconds),
        compaction_count: continuation
            .as_ref()
            .map(|state| state.compaction_count)
            .unwrap_or(0),
        pause_reason: continuation
            .as_ref()
            .and_then(|state| state.pause.as_ref())
            .map(|pause| pause.reason.as_str().to_string()),
        pause_detail: continuation
            .as_ref()
            .and_then(|state| state.pause.as_ref())
            .and_then(|pause| pause.detail.clone()),
        deferred: continuation.as_ref().is_some_and(|state| state.deferred),
        active_cell_count,
        tasks,
    });
}

fn format_budget_usage(label: &str, used: u64, budget: Option<u64>, unit: &str) -> String {
    match budget {
        Some(budget) => format!(
            "{label}: {used}{unit} used | {budget}{unit} budget | {}{unit} remaining",
            budget.saturating_sub(used)
        ),
        None => format!("{label}: {used}{unit} used | unbounded"),
    }
}

fn parse_tui_budget(value: &str, label: &str) -> Result<Option<u64>, String> {
    if matches!(value, "none" | "unbounded") {
        return Ok(None);
    }
    let budget = value
        .parse::<u64>()
        .map_err(|error| format!("invalid {label} budget: {error}"))?;
    if budget == 0 {
        return Err(format!("{label} budget must be positive or 'none'"));
    }
    Ok(Some(budget))
}

fn format_pause_reason(reason: &str) -> &str {
    match reason {
        "user" => "paused by user",
        "needs_input" => "input required",
        "approval" => "approval required",
        "boot_recovery" => "restart recovery required",
        "usage_limit" => "provider usage limit reached",
        "token_budget" => "token budget exhausted",
        "time_budget" => "time budget exhausted",
        "repeated_blocker" => "repeated blocker detected",
        "indeterminate_side_effect" => "side effect needs review",
        "provider_unavailable" => "provider unavailable",
        unknown => unknown,
    }
}

fn format_task_runtime_view(view: &TaskRuntimeView) -> String {
    let mut content = format!(
        "Run {} [{}]\nGoal r{}: {}",
        view.run_id, view.status, view.goal_revision, view.goal
    );
    if view.continuation_enabled {
        let turn = view
            .turn_ordinal
            .map(|ordinal| ordinal.to_string())
            .unwrap_or_else(|| "not started".to_string());
        content.push_str(&format!(
            "\nContinuation: active | turn: {turn} | compactions: {} | active cells: {}{}",
            view.compaction_count,
            view.active_cell_count,
            if view.deferred {
                " | waiting for cell"
            } else {
                ""
            }
        ));
        content.push_str(&format!(
            "\n{}",
            format_budget_usage("Tokens", view.tokens_used, view.token_budget, "")
        ));
        content.push_str(&format!(
            "\n{}",
            format_budget_usage(
                "Time",
                view.time_used_seconds,
                view.time_budget_seconds,
                "s"
            )
        ));
    }
    if let Some(reason) = &view.pause_reason {
        content.push_str(&format!("\nPause reason: {}", format_pause_reason(reason)));
        if let Some(detail) = view
            .pause_detail
            .as_deref()
            .filter(|detail| !detail.is_empty())
        {
            content.push_str(&format!("\nPause detail: {detail}"));
        }
    }
    if view.tasks.is_empty() {
        content.push_str("\nPlan: not created yet");
        return content;
    }
    content.push_str("\nPlan:");
    for task in &view.tasks {
        content.push_str(&format!(
            "\n  [{}] {} ({})",
            task.status, task.title, task.agent_role
        ));
    }
    content
}

fn append_subagent_summary(content: &mut String, runs: &[SubagentRuntimeView]) {
    if runs.is_empty() {
        return;
    }
    content.push_str("\nSubagents:");
    for run in runs.iter().rev().take(10).rev() {
        let usage = run
            .tokens_used
            .map(|tokens| format!(", {tokens} tokens"))
            .unwrap_or_default();
        let summary = if run.summary.trim().is_empty() {
            String::new()
        } else {
            format!(" · {}", run.summary.chars().take(160).collect::<String>())
        };
        content.push_str(&format!(
            "\n  [{}] {} · {} tools{}{}",
            run.status, run.agent, run.tool_calls, usage, summary
        ));
        if !run.verification.is_empty() {
            content.push_str(&format!(
                "\n    verification: {}",
                run.verification.join("; ")
            ));
        }
        if !run.artifacts.is_empty() {
            content.push_str(&format!("\n    artifacts: {}", run.artifacts.join(", ")));
        }
        if !run.remaining_work.is_empty() {
            content.push_str(&format!(
                "\n    remaining: {}",
                run.remaining_work.join("; ")
            ));
        }
    }
}

fn parse_hook_event(name: &str) -> Option<echo_agent::skills::hooks::HookEvent> {
    echo_agent::skills::hooks::HookEvent::from_name(name)
}

fn with_plugin_wiring_errors(
    mut message: String,
    summary: &echo_agent_app_core::plugin_runtime::ReloadSummary,
) -> String {
    if !summary.errors.is_empty() {
        message.push_str("\nComponent wiring errors:\n");
        message.push_str(&summary.errors.join("\n"));
    }
    message
}

async fn sync_tui_plugin_theme(
    app: &mut TuiApp,
    runtime: &echo_agent_app_core::plugin_runtime::PluginRuntimeService,
) {
    let active = runtime.active_theme().await;
    let theme = match active {
        Some(active) => runtime
            .themes()
            .await
            .into_iter()
            .find(|theme| theme.name == active)
            .map_or_else(
                || app.default_theme.clone(),
                |theme| crate::tui::Theme::from_plugin_theme(&theme),
            ),
        None => app.default_theme.clone(),
    };
    app.theme = theme;
    app.rebuild_message_groups();
}

async fn handle_tui_plugin_command(
    app: &mut TuiApp,
    runtime: std::sync::Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>,
    args: &str,
) -> String {
    use echo_agent::plugin::{InstallSource, PluginScope};

    let parts = args.split_whitespace().collect::<Vec<_>>();
    let sub = parts.first().copied().unwrap_or("list");
    let rest = parts.get(1..).unwrap_or(&[]);
    match sub {
        "list" | "ls" | "" => {
            let plugins = runtime.list().await;
            if plugins.is_empty() {
                return "No plugins installed.".to_string();
            }
            plugins
                .into_iter()
                .map(|entry| {
                    format!(
                        "{} v{} [{}] · {}",
                        entry.manifest.name,
                        entry.manifest.version_label(),
                        if entry.enabled { "enabled" } else { "disabled" },
                        entry.scope
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        "install" => {
            let Some(source_text) = rest.first() else {
                return "Usage: /plugins install <path|git-url> [--scope user|project|local]"
                    .to_string();
            };
            let scope = rest
                .windows(2)
                .find(|pair| pair.first() == Some(&"--scope"))
                .and_then(|pair| pair.get(1))
                .and_then(|value| PluginScope::from_arg(value))
                .unwrap_or(PluginScope::User);
            match runtime
                .install(&InstallSource::parse(source_text), scope)
                .await
            {
                Ok((plugin_id, summary)) => {
                    sync_tui_plugin_theme(app, &runtime).await;
                    let enabled = runtime
                        .get(&plugin_id)
                        .await
                        .is_some_and(|entry| entry.enabled);
                    with_plugin_wiring_errors(
                        if !enabled {
                            format!("Plugin '{plugin_id}' installed and disabled by default.")
                        } else if summary.errors.is_empty() {
                            format!("Plugin '{plugin_id}' installed and activated.")
                        } else {
                            format!("Plugin '{plugin_id}' installed, but activation is incomplete.")
                        },
                        &summary,
                    )
                }
                Err(error) => format!("Plugin install failed: {error}"),
            }
        }
        "uninstall" | "remove" => {
            let Some(name) = rest.first() else {
                return "Usage: /plugins uninstall <name> [--keep-data]".to_string();
            };
            let keep_data = rest.contains(&"--keep-data");
            match runtime.uninstall(name, keep_data).await {
                Ok(summary) => {
                    sync_tui_plugin_theme(app, &runtime).await;
                    with_plugin_wiring_errors(
                        format!("Plugin '{name}' uninstalled and unloaded."),
                        &summary,
                    )
                }
                Err(error) => format!("Plugin uninstall failed: {error}"),
            }
        }
        "enable" | "disable" => {
            let Some(name) = rest.first() else {
                return format!("Usage: /plugins {sub} <name>");
            };
            let result = if sub == "enable" {
                runtime.enable(name).await
            } else {
                runtime.disable(name).await
            };
            match result {
                Ok(summary) => {
                    sync_tui_plugin_theme(app, &runtime).await;
                    with_plugin_wiring_errors(
                        format!("Plugin '{name}' {sub}d in the current session."),
                        &summary,
                    )
                }
                Err(error) => format!("Plugin {sub} failed: {error}"),
            }
        }
        "info" | "details" => {
            let Some(name) = rest.first() else {
                return "Usage: /plugins info <name>".to_string();
            };
            match runtime.get(name).await {
                Some(entry) => {
                    let capabilities =
                        echo_agent_app_core::plugin_runtime::plugin_capabilities(&entry)
                        .into_iter()
                        .map(|capability| capability.display_name().to_string())
                        .collect::<Vec<_>>();
                    format!(
                        "{} v{}\n{}\nScope: {}\nEnabled: {}\nPath: {}\nCapabilities: {}",
                        entry.manifest.name,
                        entry.manifest.version_label(),
                        entry.manifest.description,
                        entry.scope,
                        entry.enabled,
                        entry.root.display(),
                        if capabilities.is_empty() {
                            "none".to_string()
                        } else {
                            capabilities.join(", ")
                        }
                    )
                }
                None => format!("Plugin '{name}' not found."),
            }
        }
        "reload" => match runtime.reload().await {
            Ok(summary) => {
                sync_tui_plugin_theme(app, &runtime).await;
                let mut content = format!(
                    "Reloaded {} plugins ({} enabled).\nSkills: {} · Hooks: {} · MCP: {} · Agents: {} · LSP: {} · Monitors: {} · Themes: {} · Styles: {}",
                    summary.total,
                    summary.enabled,
                    summary.skills_loaded,
                    summary.hooks_registered,
                    summary.mcp_connected,
                    summary.agents_loaded,
                    summary.lsp_languages_loaded,
                    summary.monitors_loaded,
                    summary.themes_loaded,
                    summary.output_styles_loaded
                );
                if !summary.errors.is_empty() {
                    content.push_str("\nErrors:\n");
                    content.push_str(&summary.errors.join("\n"));
                }
                content
            }
            Err(error) => format!("Plugin reload failed: {error}"),
        },
        "themes" => {
            let active = runtime.active_theme().await;
            let themes = runtime.themes().await;
            if themes.is_empty() {
                "No plugin themes are loaded.".to_string()
            } else {
                themes
                    .into_iter()
                    .map(|theme| {
                        format!(
                            "{}{} [{}] from {}",
                            if active.as_deref() == Some(theme.name.as_str()) { "* " } else { "  " },
                            theme.display_name.as_deref().unwrap_or(&theme.name),
                            if theme.dark { "dark" } else { "light" },
                            theme.plugin
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        "theme" => {
            let Some(name) = rest.first().copied() else {
                return "Usage: /plugins theme <name|default>".to_string();
            };
            let selected = (!matches!(name, "default" | "off" | "none")).then_some(name);
            match runtime.activate_theme(selected).await {
                Ok(_) => {
                    sync_tui_plugin_theme(app, &runtime).await;
                    match selected {
                        Some(name) => format!("Theme '{name}' activated."),
                        None => "Theme reset to default.".to_string(),
                    }
                }
                Err(error) => format!("Theme activation failed: {error}"),
            }
        }
        "config" | "configure" => {
            let Some(name) = rest.first().copied() else {
                return "Usage: /plugins config <name> <json-object>".to_string();
            };
            let json = rest.get(1..).unwrap_or(&[]).join(" ");
            let values = match serde_json::from_str::<
                std::collections::HashMap<String, serde_json::Value>,
            >(&json) {
                Ok(values) => values,
                Err(error) => return format!("Plugin config JSON is invalid: {error}"),
            };
            match runtime.configure(name, values).await {
                Ok(summary) => {
                    sync_tui_plugin_theme(app, &runtime).await;
                    with_plugin_wiring_errors(
                        format!("Plugin '{name}' configured and reloaded."),
                        &summary,
                    )
                }
                Err(error) => format!("Plugin configuration failed: {error}"),
            }
        }
        "styles" => {
            let active = runtime.active_output_style().await;
            let styles = runtime.output_styles().await;
            if styles.is_empty() {
                "No plugin output styles are loaded.".to_string()
            } else {
                styles
                    .into_iter()
                    .map(|style| {
                        format!(
                            "{}{} from {} - {}",
                            if active.as_deref() == Some(style.name.as_str()) {
                                "* "
                            } else {
                                "  "
                            },
                            style.name,
                            style.plugin,
                            style.description
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        "style" => {
            let Some(name) = rest.first().copied() else {
                return "Usage: /plugins style <name|default>".to_string();
            };
            let selected = (!matches!(name, "default" | "off" | "none")).then_some(name);
            match runtime.activate_output_style(selected).await {
                Ok(()) => match selected {
                    Some(name) => format!("Output style '{name}' activated."),
                    None => "Output style reset to default.".to_string(),
                },
                Err(error) => format!("Output style activation failed: {error}"),
            }
        }
        "init" => {
            let Some(directory) = rest.first().copied() else {
                return "Usage: /plugins init <directory> [name]".to_string();
            };
            let default_name = std::path::Path::new(directory)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("my-plugin");
            let name = rest.get(1).copied().unwrap_or(default_name);
            match echo_agent_app_core::plugin_runtime::PluginRuntimeService::scaffold(
                directory, name,
            ) {
                Ok(result) => format!(
                    "Plugin '{}' scaffolded at {}.",
                    result.name,
                    result.path.display()
                ),
                Err(error) => format!("Plugin scaffold failed: {error}"),
            }
        }
        "validate" => {
            let directory = rest.first().copied().unwrap_or(".");
            let report =
                echo_agent_app_core::plugin_runtime::PluginRuntimeService::validate(directory);
            if report.valid {
                format!(
                    "Plugin '{}' is valid.\nComponents: {}",
                    report.name.as_deref().unwrap_or("<unknown>"),
                    report.components.join(", ")
                )
            } else {
                format!("Plugin validation failed:\n{}", report.errors.join("\n"))
            }
        }
        _ => "Usage: /plugins [list|install|uninstall|enable|disable|info|reload|config|themes|theme|styles|style|init|validate]".to_string(),
    }
}

fn update_subagent_runs(app: &mut TuiApp, event: &SubagentEvent) {
    match event {
        SubagentEvent::DispatchStarted {
            agent,
            task,
            execution_id,
            background,
            ..
        } => {
            let id = subagent_event_id(execution_id.as_deref(), agent);
            if let Some(run) = app
                .subagent_runs
                .iter_mut()
                .find(|run| run.execution_id == id)
            {
                run.agent = agent.clone();
                run.task = task.clone();
                run.status = "running".to_string();
                run.background = *background;
            } else {
                app.subagent_runs.push(SubagentRuntimeView {
                    execution_id: id,
                    agent: agent.clone(),
                    task: task.clone(),
                    status: "running".to_string(),
                    tool_calls: 0,
                    tokens_used: None,
                    duration_ms: None,
                    background: *background,
                    summary: String::new(),
                    artifacts: Vec::new(),
                    verification: Vec::new(),
                    remaining_work: Vec::new(),
                    files_read: Vec::new(),
                    files_written: Vec::new(),
                });
            }
        }
        SubagentEvent::DispatchToolStarted {
            agent,
            execution_id,
            ..
        } => {
            if let Some(run) = find_subagent_run_mut(app, execution_id.as_deref(), agent) {
                run.tool_calls = run.tool_calls.saturating_add(1);
            }
        }
        SubagentEvent::DispatchCompleted {
            agent,
            execution_id,
            duration_ms,
            tokens_used,
            result,
            ..
        } => {
            if let Some(run) = find_subagent_run_mut(app, execution_id.as_deref(), agent) {
                run.status = result.status.as_str().to_string();
                run.duration_ms = Some(*duration_ms);
                run.tokens_used = *tokens_used;
                apply_subagent_result(run, result);
            }
        }
        SubagentEvent::DispatchFailed {
            agent,
            execution_id,
            status,
            result,
            ..
        } => {
            if let Some(run) = find_subagent_run_mut(app, execution_id.as_deref(), agent) {
                run.status = status.as_str().to_string();
                apply_subagent_result(run, result);
            }
        }
        SubagentEvent::DispatchCancelled {
            agent,
            execution_id,
            result,
            ..
        } => {
            if let Some(run) = find_subagent_run_mut(app, execution_id.as_deref(), agent) {
                run.status = "cancelled".to_string();
                apply_subagent_result(run, result);
            }
        }
        _ => {}
    }
    if app.subagent_runs.len() > 50 {
        let remove = app.subagent_runs.len().saturating_sub(50);
        app.subagent_runs.drain(..remove);
    }
}

fn apply_subagent_result(
    run: &mut SubagentRuntimeView,
    result: &echo_agent::agent::subagent::SubagentOutcome,
) {
    run.summary = result.summary.clone();
    run.artifacts = result
        .artifacts
        .iter()
        .map(|artifact| artifact.path.clone())
        .collect();
    run.verification = result
        .verification
        .iter()
        .map(|item| format!("{}: {:?}", item.check, item.status))
        .collect();
    run.remaining_work = result.remaining_work.clone();
    run.files_read = result.touched_files.read.clone();
    run.files_written = result.touched_files.written.clone();
}

fn subagent_event_id(execution_id: Option<&str>, agent: &str) -> String {
    execution_id.unwrap_or(agent).to_string()
}

fn find_subagent_run_mut<'a>(
    app: &'a mut TuiApp,
    execution_id: Option<&str>,
    agent: &str,
) -> Option<&'a mut SubagentRuntimeView> {
    let id = subagent_event_id(execution_id, agent);
    app.subagent_runs
        .iter_mut()
        .rev()
        .find(|run| run.execution_id == id)
}

// ── Parallel task progress strip ────────────────────────────────────────

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
    use super::{
        TurnDispatchResult, apply_turn_settlement, complete_file_reference, delete_previous_word,
        format_task_runtime_view, format_unattended_worktrees, handle_approval_key, handle_esc,
        move_cursor_vertical, parse_interaction_mode, project_queued_dispatch,
        queue_steer_follow_up, render_cancelled_event, render_error_event,
        resolve_tui_workspace_file, retry_tui_task, reverse_history_search,
        run_turn_binding_for_queued_turn, slash_command_allowed_while_busy, update_subagent_runs,
    };
    use crate::tui::{
        ChatMessage, MessageRole, QueuedRunResume, QueuedTurn, TaskRuntimeTaskView,
        TaskRuntimeView, Theme, ToolExecutionMessage, ToolExecutionStatus, TuiApp,
    };
    use echo_agent_app_core::chat_driver::TurnOutcome;
    use echo_agent_app_core::tasks::task_runtime::{
        AttendedMode, DomainProfile, ExecutionMode, InteractionMode, PlanTask, TaskPlan,
        TaskRunStatus, TaskRuntimeStore, TodoStatus, commit_eko_task_plan,
    };
    use std::sync::Arc;
    use std::time::Instant;

    fn app() -> TuiApp {
        let theme =
            Theme::from_color_theme(&echo_agent_app_core::output::theme::ColorTheme::dark());
        TuiApp::new("test-model".to_string(), "test".to_string(), theme)
    }

    fn task_test_agent() -> Result<echo_agent::agent::AgentHandle, String> {
        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("test-model")
                .with_response("done"),
        );
        echo_agent::agent::ReactAgentBuilder::new()
            .model("test-model")
            .llm_client(llm)
            .build()
            .map(echo_agent::agent::AgentHandle::new)
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn task_retry_keeps_runtime_unchanged_when_driver_admission_is_closed()
    -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run(
                "tui-retry-closed",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "preserve retry state",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        commit_eko_task_plan(
            store.clone(),
            TaskPlan {
                plan_id: "tui-retry-plan".to_string(),
                run_id: "tui-retry-closed".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: echo_agent_app_core::tasks::task_runtime::task_goal_sha256(
                    "preserve retry state",
                ),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "retry-task".to_string(),
                    title: "Retry task".to_string(),
                    max_retries: 2,
                    ..PlanTask::default()
                }],
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        store
            .transition_run("tui-retry-closed", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "tui-retry-closed",
                "retry-task",
                TodoStatus::Failed,
                None,
                Some("acceptance failed"),
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("tui-retry-closed", TaskRunStatus::Failed)
            .map_err(|error| error.to_string())?;
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;

        let events_before = serde_json::to_value(
            store
                .list_events("tui-retry-closed", 0)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let run_before = serde_json::to_value(
            store
                .get_run("tui-retry-closed")
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let plan_before = serde_json::to_value(
            store
                .get_plan("tui-retry-closed")
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        let error = retry_tui_task(
            store.clone(),
            task_test_agent()?,
            "tui-retry-closed".to_string(),
            "retry-task".to_string(),
            None,
        )
        .await
        .err()
        .ok_or_else(|| "TUI retry unexpectedly bypassed closed admission".to_string())?;
        assert!(error.to_string().contains("task runtime is shutting down"));
        assert_eq!(
            events_before,
            serde_json::to_value(
                store
                    .list_events("tui-retry-closed", 0)
                    .map_err(|error| error.to_string())?
            )
            .map_err(|error| error.to_string())?
        );
        assert_eq!(
            run_before,
            serde_json::to_value(
                store
                    .get_run("tui-retry-closed")
                    .map_err(|error| error.to_string())?
            )
            .map_err(|error| error.to_string())?
        );
        assert_eq!(
            plan_before,
            serde_json::to_value(
                store
                    .get_plan("tui-retry-closed")
                    .map_err(|error| error.to_string())?
            )
            .map_err(|error| error.to_string())?
        );
        Ok(())
    }

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

    #[test]
    fn only_matching_settlement_releases_the_tui_slot_once() {
        let mut app = app();
        app.is_processing = true;
        app.active_turn_id = Some("turn-1".to_string());
        app.queued_turns.push_back(QueuedTurn {
            text: "next".to_string(),
            attachments: Vec::new(),
            interaction_mode: InteractionMode::Auto,
            run_resume: None,
        });

        assert!(!apply_turn_settlement(
            &mut app,
            "stale-turn",
            &TurnOutcome::Completed
        ));
        assert!(app.is_processing);
        assert_eq!(app.active_turn_id.as_deref(), Some("turn-1"));
        assert_eq!(app.queued_turns.len(), 1);

        assert!(apply_turn_settlement(
            &mut app,
            "turn-1",
            &TurnOutcome::Cancelled
        ));
        assert!(!app.is_processing);
        assert!(app.active_turn_id.is_none());
        assert_eq!(app.status_msg, "Cancelled");
        assert_eq!(app.queued_turns.len(), 1);

        assert!(!apply_turn_settlement(
            &mut app,
            "turn-1",
            &TurnOutcome::Cancelled
        ));
        assert_eq!(app.queued_turns.len(), 1);
    }

    #[test]
    fn queued_resume_builds_exact_run_turn_binding() -> Result<(), String> {
        let turn = QueuedTurn {
            text: "continue".to_string(),
            attachments: Vec::new(),
            interaction_mode: InteractionMode::Task,
            run_resume: Some(QueuedRunResume {
                run_id: "exact-run".to_string(),
                root_message_id: "exact-root-message".to_string(),
            }),
        };

        let binding = run_turn_binding_for_queued_turn(&turn, "new-turn")
            .ok_or_else(|| "resume binding missing".to_string())?;
        assert_eq!(binding.run_id.as_deref(), Some("exact-run"));
        assert_eq!(binding.turn_id, "new-turn");
        assert_eq!(binding.root_message_id, "exact-root-message");
        assert_eq!(
            binding.origin,
            echo_agent_app_core::tasks::task_runtime::RunTurnOrigin::Resume
        );
        assert_eq!(
            binding.transcript_visibility,
            echo_agent_app_core::tasks::task_runtime::TurnVisibility::Visible
        );
        Ok(())
    }

    #[test]
    fn retryable_fifo_rejection_preserves_head_attachments_and_editor_draft() {
        let mut app = app();
        app.input = "draft still being edited".to_string();
        app.cursor = app.input.len();
        let attachment = echo_agent_app_core::attachments::AttachmentRef {
            path: std::path::PathBuf::from("/tmp/queued.txt"),
            name: "queued.txt".to_string(),
            mime_type: "text/plain".to_string(),
            source: echo_agent_app_core::types::AttachmentSource::Upload,
        };
        let first = QueuedTurn {
            text: "first".to_string(),
            attachments: vec![attachment],
            interaction_mode: InteractionMode::Auto,
            run_resume: None,
        };
        let second = QueuedTurn {
            text: "second".to_string(),
            attachments: Vec::new(),
            interaction_mode: InteractionMode::Auto,
            run_resume: None,
        };
        app.queued_turns.push_back(second);

        project_queued_dispatch(
            &mut app,
            TurnDispatchResult::Rejected {
                turn: first,
                error: "workspace transition".to_string(),
                retryable: true,
            },
        );

        assert_eq!(app.input, "draft still being edited");
        assert_eq!(app.cursor, app.input.len());
        assert_eq!(app.queued_turns.len(), 2);
        assert_eq!(
            app.queued_turns.front().map(|turn| turn.text.as_str()),
            Some("first")
        );
        assert_eq!(
            app.queued_turns.front().map(|turn| turn.attachments.len()),
            Some(1)
        );
        assert_eq!(
            app.queued_turns.get(1).map(|turn| turn.text.as_str()),
            Some("second")
        );
    }

    #[test]
    fn steer_settlement_race_queues_text_and_attachments_once() {
        let mut app = app();
        let attachment = echo_agent_app_core::attachments::AttachmentRef {
            path: std::path::PathBuf::from("/tmp/steer.txt"),
            name: "steer.txt".to_string(),
            mime_type: "text/plain".to_string(),
            source: echo_agent_app_core::types::AttachmentSource::Upload,
        };

        queue_steer_follow_up(&mut app, "continue with this", vec![attachment]);

        assert_eq!(app.queued_turns.len(), 1);
        assert_eq!(
            app.queued_turns.front().map(|turn| turn.text.as_str()),
            Some("continue with this")
        );
        assert_eq!(
            app.queued_turns.front().map(|turn| turn.attachments.len()),
            Some(1)
        );
    }

    #[test]
    fn terminal_stream_events_render_without_releasing_the_tui_slot() {
        let mut app = app();
        app.is_processing = true;
        app.active_turn_id = Some("turn-1".to_string());

        render_cancelled_event(&mut app);
        assert!(app.is_processing);
        assert_eq!(app.active_turn_id.as_deref(), Some("turn-1"));

        render_error_event(&mut app, "provider failed");
        assert!(app.is_processing);
        assert_eq!(app.active_turn_id.as_deref(), Some("turn-1"));
        assert!(
            app.messages
                .iter()
                .any(|message| message.content == "Error: provider failed")
        );
    }

    #[test]
    fn completed_settlement_converges_a_missing_tool_terminal_event() {
        let mut app = app();
        app.is_processing = true;
        app.active_turn_id = Some("turn-1".to_string());
        app.messages.push(ChatMessage {
            role: MessageRole::ToolExecution(Box::new(ToolExecutionMessage {
                call_id: "call-1".to_string(),
                name: "read_file".to_string(),
                args: "{}".to_string(),
                status: ToolExecutionStatus::Running,
                stdout: String::new(),
                stderr: String::new(),
                log: String::new(),
                progress: None,
                truncated: false,
                started_at: Instant::now(),
                finished_at: None,
                metadata: std::collections::HashMap::new(),
            })),
            content: String::new(),
        });

        assert!(apply_turn_settlement(
            &mut app,
            "turn-1",
            &TurnOutcome::Completed
        ));
        assert!(!app.has_running_tools());
        assert!(app.messages.iter().any(|message| {
            matches!(
                &message.role,
                MessageRole::ToolExecution(tool)
                    if tool.status == ToolExecutionStatus::Failed
                        && tool.stderr.contains("terminal event missing")
            )
        }));
    }

    #[tokio::test]
    async fn consecutive_hitl_inputs_advance_the_front_immediately() -> Result<(), String> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
        use echo_agent_app_core::hitl::TuiHumanLoopProvider;

        let provider = std::sync::Arc::new(TuiHumanLoopProvider::new());
        let first_provider = provider.clone();
        let mut first = HumanLoopRequest::input("First");
        first.request_id = Some("request-1".to_string());
        let first_task = tokio::spawn(async move { first_provider.request(first).await });
        let pending = provider.pending_handle();
        wait_for_tui_pending_count(&pending, 1).await?;

        let second_provider = provider.clone();
        let mut second = HumanLoopRequest::input("Second");
        second.request_id = Some("request-2".to_string());
        let second_task = tokio::spawn(async move { second_provider.request(second).await });
        wait_for_tui_pending_count(&pending, 2).await?;

        let mut app = app();
        assert!(
            handle_approval_key(
                &mut app,
                &pending,
                &KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            )
            .await
        );
        assert!(
            handle_approval_key(
                &mut app,
                &pending,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            )
            .await
        );
        {
            let state = pending.lock().map_err(|error| error.to_string())?;
            assert_eq!(
                state.front().map(|request| request.request_id.as_str()),
                Some("request-2")
            );
        }

        assert!(
            handle_approval_key(
                &mut app,
                &pending,
                &KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
            )
            .await
        );
        assert!(
            handle_approval_key(
                &mut app,
                &pending,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            )
            .await
        );
        assert!(
            pending
                .lock()
                .map_err(|error| error.to_string())?
                .is_empty()
        );

        let first_response = first_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let second_response = second_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(first_response, HumanLoopResponse::Text(text) if text == "a"));
        assert!(matches!(second_response, HumanLoopResponse::Text(text) if text == "b"));
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_hitl_front_exposes_the_next_request_on_input() -> Result<(), String> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
        use echo_agent_app_core::hitl::TuiHumanLoopProvider;

        let provider = std::sync::Arc::new(TuiHumanLoopProvider::new());
        let first_provider = provider.clone();
        let mut first = HumanLoopRequest::input("First");
        first.request_id = Some("request-1".to_string());
        let first_task = tokio::spawn(async move { first_provider.request(first).await });
        let pending = provider.pending_handle();
        wait_for_tui_pending_count(&pending, 1).await?;

        let second_provider = provider.clone();
        let mut second = HumanLoopRequest::input("Second");
        second.request_id = Some("request-2".to_string());
        let second_task = tokio::spawn(async move { second_provider.request(second).await });
        wait_for_tui_pending_count(&pending, 2).await?;

        first_task.abort();
        let _ = first_task.await;
        assert_eq!(pending.lock().map_err(|error| error.to_string())?.len(), 1);

        let mut app = app();
        assert!(
            handle_approval_key(
                &mut app,
                &pending,
                &KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
            )
            .await
        );
        {
            let queue = pending.lock().map_err(|error| error.to_string())?;
            let front = queue
                .front()
                .ok_or_else(|| "next request was not exposed".to_string())?;
            assert_eq!(front.request_id, "request-2");
            assert_eq!(front.feedback_input, "z");
        }
        assert!(
            handle_approval_key(
                &mut app,
                &pending,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            )
            .await
        );
        assert!(
            pending
                .lock()
                .map_err(|error| error.to_string())?
                .is_empty()
        );

        let response = second_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(response, HumanLoopResponse::Text(text) if text == "z"));
        Ok(())
    }

    async fn wait_for_tui_pending_count(
        pending: &echo_agent_app_core::hitl::PendingApprovalQueue,
        expected: usize,
    ) -> Result<(), String> {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if pending.lock().map(|state| state.len()).unwrap_or_default() == expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| format!("pending request count did not reach {expected}"))
    }

    #[test]
    fn multiline_cursor_moves_without_breaking_utf8() {
        let mut app = app();
        app.input = "中文abc\n第二行".to_string();
        app.cursor = app.input.len();

        assert!(move_cursor_vertical(&mut app, -1));
        assert!(app.input.is_char_boundary(app.cursor));
        assert!(app.cursor < app.input.len());

        assert!(move_cursor_vertical(&mut app, 1));
        assert_eq!(app.cursor, app.input.len());
    }

    #[test]
    fn delete_previous_word_is_utf8_safe() {
        let mut app = app();
        app.input = "保留 删除我".to_string();
        app.cursor = app.input.len();

        delete_previous_word(&mut app);

        assert_eq!(app.input, "保留 ");
        assert_eq!(app.cursor, app.input.len());
    }

    #[test]
    fn busy_command_allowlist_is_explicit() {
        assert!(slash_command_allowed_while_busy("/status"));
        assert!(slash_command_allowed_while_busy("/tasks"));
        assert!(slash_command_allowed_while_busy("/steer focus on tests"));
        assert!(!slash_command_allowed_while_busy("/clear"));
        assert!(!slash_command_allowed_while_busy("/model other"));
    }

    #[test]
    fn formats_retained_worktree_review_facts() {
        let worktree = echo_agent_app_core::tasks::task_runtime::worktree::UnattendedWorktreeInfo {
            run_id: "run-123".to_string(),
            branch: "eko-unattended-run-123".to_string(),
            path: None,
            head: "abc123".to_string(),
            status: "completed".to_string(),
            active: false,
            locked: true,
            lock_reason: Some("in progress".to_string()),
            uncommitted_changes: false,
            ahead_commits: 0,
            has_changes: false,
            orphan_branch: true,
        };

        let formatted = format_unattended_worktrees(&[worktree]);
        assert!(formatted.contains("unchanged,stale-lock,orphan-branch"));
        assert!(formatted.contains("no checkout"));
    }

    #[tokio::test]
    async fn repeated_idle_escape_requests_rewind() {
        let mut app = app();

        handle_esc(&mut app).await;
        assert!(!app.rewind_requested);
        handle_esc(&mut app).await;

        assert!(app.rewind_requested);
        assert!(app.last_escape_at.is_none());
    }

    #[test]
    fn reverse_search_walks_earlier_matches() {
        let mut app = app();
        app.history = vec![
            "first build".to_string(),
            "run tests".to_string(),
            "second build".to_string(),
        ];
        app.input = "build".to_string();
        app.cursor = app.input.len();

        reverse_history_search(&mut app);
        assert_eq!(app.input, "second build");
        reverse_history_search(&mut app);
        assert_eq!(app.input, "first build");
    }

    #[test]
    fn file_reference_completion_keeps_utf8_boundary() {
        let mut app = app();
        app.project_files = vec!["src/中文.rs".to_string()];
        app.input = "检查 @中文".to_string();
        app.cursor = app.input.len();

        assert!(complete_file_reference(&mut app));
        assert_eq!(app.input, "检查 @src/中文.rs");
        assert!(app.input.is_char_boundary(app.cursor));
    }

    #[test]
    fn tui_workspace_file_resolution_accepts_repo_files_and_rejects_empty_input() {
        assert!(resolve_tui_workspace_file("Cargo.toml").is_ok());
        assert!(resolve_tui_workspace_file("").is_err());
    }

    #[test]
    fn task_runtime_projection_formats_plan_state() {
        let view = TaskRuntimeView {
            run_id: "run-1".to_string(),
            goal: "补齐 TUI".to_string(),
            goal_revision: 3,
            status: "running".to_string(),
            continuation_enabled: true,
            turn_ordinal: Some(4),
            tokens_used: 12_345,
            token_budget: Some(50_000),
            time_used_seconds: 90,
            time_budget_seconds: Some(300),
            compaction_count: 2,
            pause_reason: None,
            pause_detail: None,
            deferred: false,
            active_cell_count: 1,
            tasks: vec![TaskRuntimeTaskView {
                title: "实现队列".to_string(),
                status: "completed".to_string(),
                agent_role: "implementer".to_string(),
            }],
        };

        let text = format_task_runtime_view(&view);
        assert!(text.contains("run-1 [running]"));
        assert!(text.contains("Continuation: active | turn: 4 | compactions: 2"));
        assert!(text.contains("Tokens: 12345 used | 50000 budget | 37655 remaining"));
        assert!(text.contains("Time: 90s used | 300s budget | 210s remaining"));
        assert!(text.contains("[completed] 实现队列 (implementer)"));
    }

    #[test]
    fn task_runtime_projection_formats_exhausted_time_budget_without_underflow() {
        let view = TaskRuntimeView {
            run_id: "run-time-budget".to_string(),
            goal: "finish within the configured time".to_string(),
            goal_revision: 1,
            status: "paused".to_string(),
            continuation_enabled: true,
            turn_ordinal: Some(8),
            tokens_used: 900,
            token_budget: None,
            time_used_seconds: 305,
            time_budget_seconds: Some(300),
            compaction_count: 1,
            pause_reason: Some("time_budget".to_string()),
            pause_detail: Some("configured limit reached".to_string()),
            deferred: false,
            active_cell_count: 0,
            tasks: Vec::new(),
        };

        let text = format_task_runtime_view(&view);
        assert!(text.contains("Tokens: 900 used | unbounded"));
        assert!(text.contains("Time: 305s used | 300s budget | 0s remaining"));
        assert!(text.contains("Pause reason: time budget exhausted"));
        assert!(text.contains("Pause detail: configured limit reached"));
    }

    #[test]
    fn subagent_events_update_live_projection() {
        use echo_agent::agent::subagent::{ExecutionMode, SubagentEvent};

        let mut app = app();
        update_subagent_runs(
            &mut app,
            &SubagentEvent::DispatchStarted {
                parent: "main".to_string(),
                agent: "explorer".to_string(),
                mode: ExecutionMode::Fork,
                task: "inspect TUI".to_string(),
                execution_id: Some("task-1:1".to_string()),
                run_id: Some("run-1".to_string()),
                conversation_id: Some("conversation-1".to_string()),
                message_id: None,
                background: false,
            },
        );
        update_subagent_runs(
            &mut app,
            &SubagentEvent::DispatchToolStarted {
                parent: "main".to_string(),
                agent: "explorer".to_string(),
                call_id: "call-1".to_string(),
                name: "read_file".to_string(),
                args: serde_json::json!({}),
                execution_id: Some("task-1:1".to_string()),
                run_id: Some("run-1".to_string()),
            },
        );
        let terminal_result = echo_agent::agent::subagent::SubagentOutcome {
            contract_version: 1,
            status: echo_agent::agent::subagent::SubagentStatus::Completed,
            summary: "done".to_string(),
            artifacts: vec![echo_agent::agent::subagent::SubagentArtifact {
                path: "report.json".to_string(),
                kind: "report".to_string(),
                bytes: Some(42),
                sha256: Some("a".repeat(64)),
                producer_execution_id: Some("task-1:1".to_string()),
                available: true,
            }],
            verification: vec![echo_agent::agent::subagent::SubagentVerification {
                check: "cargo test".to_string(),
                status: echo_agent::agent::subagent::SubagentVerificationStatus::Passed,
                details: "ok".to_string(),
                source: echo_agent::agent::subagent::SubagentVerificationSource::Observed,
            }],
            remaining_work: Vec::new(),
            touched_files: echo_agent::agent::subagent::SubagentTouchedFiles {
                read: vec!["src/lib.rs".to_string()],
                written: vec!["report.json".to_string()],
            },
        };
        update_subagent_runs(
            &mut app,
            &SubagentEvent::DispatchCompleted {
                parent: "main".to_string(),
                agent: "explorer".to_string(),
                duration_ms: 120,
                tokens_used: Some(42),
                iterations: Some(1),
                output: "done".to_string(),
                result: terminal_result,
                execution_id: Some("task-1:1".to_string()),
                run_id: Some("run-1".to_string()),
            },
        );

        let run = app.subagent_runs.first().cloned().unwrap_or_default();
        assert_eq!(run.status, "completed");
        assert_eq!(run.tool_calls, 1);
        assert_eq!(run.tokens_used, Some(42));
        assert_eq!(run.duration_ms, Some(120));
        assert_eq!(run.summary, "done");
        assert_eq!(run.artifacts, vec!["report.json".to_string()]);
        assert_eq!(run.verification, vec!["cargo test: Passed".to_string()]);
        assert_eq!(run.files_read, vec!["src/lib.rs".to_string()]);
        assert_eq!(run.files_written, vec!["report.json".to_string()]);
    }
}
