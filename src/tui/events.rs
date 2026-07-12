//! TUI event loop — handles keyboard input, terminal resize, and agent streaming.

use super::{
    ChatMessage, MessageRole, QueuedTurn, SubagentRuntimeView, TaskProgressEntry,
    TaskRuntimeTaskView, TaskRuntimeView, TaskStripStatus, ToolExecutionMessage,
    ToolExecutionStatus, TuiApp,
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
use ratatui::layout::Rect;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

use echo_agent::agent::subagent::SubagentEvent;
use echo_agent::tasks::TaskEvent;
use echo_agent_app_core::context_window::ContextWindowSnapshot;
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
    },
    /// A tool execution completed.
    ToolResult {
        call_id: String,
        output: String,
        success: bool,
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
        ToolExecutionMessage, ToolExecutionStatus, tool_command, tool_detail, tool_output_tail,
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
            "plan_execute",
            r#"{"task":{"agent_role":"explorer","description":"Inspect browser events"}}"#,
            ToolExecutionStatus::Succeeded,
        );
        assert_eq!(
            tool_command(&tool.name, &tool.args),
            "Execute with explorer"
        );
        assert_eq!(tool_detail(&tool), "Inspect browser events");
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
    task_service: Option<Arc<BackgroundTaskService>>,
) -> anyhow::Result<()> {
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();

    // Subscribe to task events for the parallel task progress strip.
    let mut task_event_rx: Option<broadcast::Receiver<Arc<TaskEvent>>> =
        task_service.as_ref().map(|svc| svc.subscribe_events());
    let mut subagent_event_rx = agent
        .read(|a| a.subagent_registry().event_bus().subscribe())
        .await;
    let mut last_runtime_refresh = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);

    loop {
        // ── Drain task events into parallel_tasks ──────────────────────
        if let Some(ref mut rx) = task_event_rx {
            while let Ok(event) = rx.try_recv() {
                update_parallel_tasks(app, &event);
            }
        }
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
                    dispatch_next_queued(app, &agent, agent_tx.clone()).await;
                }
                AgentEvent::Cancelled => {
                    let now = Instant::now();
                    for message in &mut app.messages {
                        if let MessageRole::ToolExecution(tool) = &mut message.role
                            && tool.status == ToolExecutionStatus::Running
                        {
                            tool.status = ToolExecutionStatus::Cancelled;
                            tool.finished_at = Some(now);
                        }
                    }
                    app.invalidate_messages_cache();
                    app.is_processing = false;
                    app.active_cancel = None;
                    app.status_msg = "Cancelled".to_string();
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
                    metadata,
                    truncated,
                } => {
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
                } => {
                    let mut diff_tool_name = None;
                    if let Some(tool) = find_tool_mut(app, &call_id) {
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
                    let was_cancelled = app
                        .active_cancel
                        .as_ref()
                        .is_some_and(echo_agent::agent::CancellationToken::is_cancelled);
                    for message in &mut app.messages {
                        if let MessageRole::ToolExecution(tool) = &mut message.role
                            && tool.status == ToolExecutionStatus::Running
                        {
                            tool.status = if was_cancelled {
                                ToolExecutionStatus::Cancelled
                            } else {
                                ToolExecutionStatus::Failed
                            };
                            if !was_cancelled && tool.stderr.is_empty() {
                                tool.stderr = e.clone();
                            }
                            tool.finished_at = Some(Instant::now());
                        }
                    }
                    app.invalidate_messages_cache();
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: if was_cancelled {
                            "Cancelled by user.".to_string()
                        } else {
                            format!("Error: {e}")
                        },
                    });
                    app.is_processing = false;
                    app.active_cancel = None;
                    app.active_turn_id = None;
                    app.status_msg = if was_cancelled {
                        "Cancelled".to_string()
                    } else {
                        "Error".to_string()
                    };
                    dispatch_next_queued(app, &agent, agent_tx.clone()).await;
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
                Ok(Event::Paste(text)) => insert_text(app, &text),
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
        KeyCode::Char('c') => {
            if app.is_processing {
                handle_esc(app);
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
            if let Some(cancel) = &app.active_cancel {
                cancel.cancel();
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
        KeyCode::Esc => handle_esc(app),
        _ => {}
    }
}

async fn handle_enter(
    app: &mut TuiApp,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) {
    let Some(text) = app.take_input() else {
        return;
    };
    if let Some(steer_text) = text.strip_prefix("/steer ") {
        steer_active_turn(app, agent, steer_text.trim()).await;
        return;
    }
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
    };
    if app.is_processing {
        let preview: String = turn.text.chars().take(60).collect();
        app.queued_turns.push_back(turn);
        app.status_msg = format!("运行中 · 已排队 {} 条", app.queued_turns.len());
        tracing::info!(queued = app.queued_turns.len(), preview, "TUI turn queued");
        return;
    }
    dispatch_turn(app, agent, agent_tx, turn).await;
}

async fn dispatch_turn(
    app: &mut TuiApp,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
    turn: QueuedTurn,
) {
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
    app.start_turn(&turn.text);
    let multimodal = if turn.attachments.is_empty() {
        None
    } else {
        match echo_agent_app_core::attachments::build_message_from_refs(
            &turn.text,
            &turn.attachments,
        ) {
            Ok(msg) => Some(msg),
            Err(e) => {
                tracing::warn!(error = %e, "failed to build multimodal message; sending text only");
                None
            }
        }
    };
    let cancel = echo_agent::agent::CancellationToken::new();
    app.active_cancel = Some(cancel.clone());
    let turn_id = uuid::Uuid::new_v4().to_string();
    app.active_turn_id = Some(turn_id.clone());
    let sink: std::sync::Arc<dyn echo_agent_app_core::chat_driver::ChatSink> =
        std::sync::Arc::new(TuiChatSink::new(agent_tx));
    let mode_hint = match turn.interaction_mode {
                InteractionMode::Chat => Some(
                    "Chat mode — task runtime tools are unavailable in this turn. \
                     Reply directly with ordinary chat/tool usage; do not create or execute a task plan."
                        .to_string(),
                ),
                InteractionMode::Task => Some(
                    "Task mode — use formal plan execution. First create explicit PlanTask items with \
                     plan_create, then call plan_execute() with no task argument to run the DAG. \
                     Do not use plan_execute({task}) inline single-subagent dispatch in Task mode."
                        .to_string(),
                ),
                InteractionMode::Auto => Some(
                    "Auto mode — classify the request yourself. For simple chat, answer directly. \
                     For high-noise research or broad codebase inspection, you may use inline \
                     plan_execute({task}) subagents. For multi-step / multi-file / long-running work, \
                     create a formal plan with plan_create and then plan_execute()."
                        .to_string(),
                ),
    };
    let res = std::sync::Arc::new(echo_agent_app_core::chat_resources::ChatResources {
        pool: app.pool.clone(),
        store: app.task_runtime_store.clone(),
        sink,
        // TUI/GUI parity (AGENTS.md): bind this turn to the session's
        // conversation id so TaskRuntime runs + transcript projection work.
        conv_id: app.conversation_id.clone(),
        root_message_id: turn_id,
        // Bind staged refs so subagents in an autonomous run see them too.
        attachments: turn.attachments,
        cancel,
        mode_hint,
        interaction_mode: turn.interaction_mode,
        // B5.1 (TUI/GUI parity): build a layer_manager per turn from
        // review_integration so autonomous runs block-write their
        // completion memory (`taskrun:completed`). None = no review/memory
        // subsystem (writes become no-ops).
        layer_manager: app
            .review_integration
            .as_ref()
            .map(|ri| std::sync::Arc::new(ri.create_layer_manager())),
    });
    send_to_agent(agent, turn.text, multimodal, res).await;
}

async fn steer_active_turn(app: &mut TuiApp, agent: &AgentHandle, text: &str) {
    if text.is_empty() {
        app.status_msg = "Usage: /steer <补充指令>".to_string();
        return;
    }
    let Some(turn_id) = app.active_turn_id.clone() else {
        app.status_msg = "当前没有可补充的运行任务".to_string();
        return;
    };
    match agent
        .steer_input(
            Some(&turn_id),
            echo_agent::prelude::Message::user(text.to_string()),
        )
        .await
    {
        Ok(_) => {
            app.messages.push(ChatMessage {
                role: MessageRole::User,
                content: text.to_string(),
            });
            app.rebuild_message_groups();
            app.status_msg = "已补充到当前任务".to_string();
        }
        Err(echo_agent::agent::TurnSteerError::NotSteerable { .. }) => {
            app.queued_turns.push_back(QueuedTurn {
                text: text.to_string(),
                attachments: Vec::new(),
                interaction_mode: app.interaction_mode,
            });
            app.status_msg = format!("当前阶段不可插入 · 已排队 {} 条", app.queued_turns.len());
        }
        Err(error) => {
            app.status_msg = format!("补充失败: {error}");
        }
    }
}

async fn dispatch_next_queued(
    app: &mut TuiApp,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) {
    if let Some(turn) = app.queued_turns.pop_front() {
        dispatch_turn(app, agent, agent_tx, turn).await;
    }
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
            stage_attachment(&mut app.pending_attachments, &path)
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
        Ok(text) => insert_text(app, &text),
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

    disable_raw_mode()?;
    execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture)?;
    if !app.inline_mode {
        execute!(io::stdout(), LeaveAlternateScreen)?;
    }
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg("${VISUAL:-${EDITOR:-vi}} \"$1\"")
        .arg("eko-editor")
        .arg(&path)
        .status();
    let edited = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    enable_raw_mode()?;
    if app.inline_mode {
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

fn handle_esc(app: &mut TuiApp) {
    if app.is_processing {
        if let Some(cancel) = &app.active_cancel {
            cancel.cancel();
            app.status_msg = "Cancelling...".to_string();
        } else {
            app.status_msg = "Waiting for current turn to settle...".to_string();
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
    let runtime_messages = echo_agent_app_core::conversation_restore::restore_messages(&stored);
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
            },
            echo_agent::agent::AgentEvent::ToolResult {
                call_id, output, ..
            } => AgentEvent::ToolResult {
                call_id,
                output,
                success: true,
            },
            echo_agent::agent::AgentEvent::ToolError { call_id, error, .. } => {
                AgentEvent::ToolResult {
                    call_id,
                    output: error,
                    success: false,
                }
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
    // subagents that re-read it later (matches the GUI's per-workspace uploads,
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
                let runtime = app
                    .configured_models
                    .iter()
                    .find(|model| model.id == requested || model.model == requested)
                    .cloned();
                let active_model = match runtime {
                    Some(runtime) => {
                        agent
                            .write(|value| {
                                if let Some(token) = runtime.auth_token.as_deref() {
                                    value.set_llm_config(
                                        echo_agent_app_core::infra::build_llm_config(
                                            &runtime.provider,
                                            token,
                                            &runtime.model,
                                            runtime.base_url.as_deref(),
                                        ),
                                    );
                                } else {
                                    value.set_model(&runtime.model);
                                }
                                value.set_temperature(runtime.temperature);
                                value.set_max_tokens(runtime.max_tokens);
                                if let Some(limit) = runtime.context_window {
                                    value.set_token_limit(limit as usize);
                                }
                            })
                            .await;
                        if let Some(pool) = &app.pool {
                            pool.apply_runtime_model(runtime.clone()).await;
                        }
                        runtime.model
                    }
                    None => {
                        agent.write(|value| value.set_model(&requested)).await;
                        requested
                    }
                };
                app.model = active_model;
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
            let store = agent.read(|value| value.store().cloned()).await;
            let content = match store {
                Some(store) => match store.list(&["default", "memories"]).await {
                    Ok(items) if items.is_empty() => "No long-term memories.".to_string(),
                    Ok(items) => items
                        .into_iter()
                        .map(|item| format!("{}: {}", item.key, item.value))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Err(error) => format!("Failed to list memories: {error}"),
                },
                None => "No long-term memory store is configured.".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Remember) => {
            let store = agent.read(|value| value.store().cloned()).await;
            let content = match store {
                _ if args.trim().is_empty() => "Usage: /remember <fact>".to_string(),
                Some(store) => {
                    let key = uuid::Uuid::new_v4().to_string();
                    match store
                        .put(
                            &["default", "memories"],
                            &key,
                            serde_json::Value::String(args.trim().to_string()),
                        )
                        .await
                    {
                        Ok(()) => format!("Memory saved with key: {key}"),
                        Err(error) => format!("Failed to save memory: {error}"),
                    }
                }
                None => "No long-term memory store is configured.".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Forget) => {
            let store = agent.read(|value| value.store().cloned()).await;
            let content = match store {
                _ if args.trim().is_empty() => "Usage: /forget <key-or-query>".to_string(),
                Some(store) => {
                    let query = args.trim();
                    let mut keys = match store.search(&["default", "memories"], query, 20).await {
                        Ok(items) => items.into_iter().map(|item| item.key).collect::<Vec<_>>(),
                        Err(_) => Vec::new(),
                    };
                    if keys.is_empty() {
                        keys.push(query.to_string());
                    }
                    let mut removed = 0usize;
                    for key in keys {
                        if store
                            .delete(&["default", "memories"], &key)
                            .await
                            .unwrap_or(false)
                        {
                            removed = removed.saturating_add(1);
                        }
                    }
                    format!("Removed {removed} matching memory item(s).")
                }
                None => "No long-term memory store is configured.".to_string(),
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
                    "Usage: /skills [list|search|install|uninstall|info|refresh] [args]".to_string()
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
                    match agent
                        .write_async(|value| {
                            Box::pin(async move { value.load_mcp_from_file(path).await })
                        })
                        .await
                    {
                        Ok(clients) => format!("Loaded {} MCP server(s).", clients.len()),
                        Err(error) => format!("MCP load failed: {error}"),
                    }
                }
                "disconnect" if !target.is_empty() => {
                    if agent
                        .write_async(|value| {
                            let target = target.clone();
                            Box::pin(async move { value.disconnect_mcp(&target).await })
                        })
                        .await
                    {
                        format!("Disconnected MCP server: {target}")
                    } else {
                        format!("MCP server '{target}' is not connected.")
                    }
                }
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
                    let loaded = echo_agent_app_core::hooks_config::load_hooks_files();
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
                "test" if !target.is_empty() => match parse_hook_event(target) {
                    Some(event) => {
                        let has = agent
                            .read_async(|value| {
                                Box::pin(async move {
                                    value.hook_registry().read().await.has_hooks_for(event)
                                })
                            })
                            .await;
                        format!("Hooks for {target}: {}", if has { "yes" } else { "no" })
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
        Some(SlashCommand::Cost) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "Token usage: prompt={}, completion={}, requests={}",
                    app.tokens.0, app.tokens.1, app.tokens.2
                ),
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
            let message = if attachments.is_empty() {
                echo_agent::llm::types::Message::user(instruction.to_string())
            } else {
                match echo_agent_app_core::attachments::build_message_from_refs(
                    instruction,
                    &attachments,
                ) {
                    Ok(message) => message,
                    Err(error) => {
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!("Failed to build steer attachment: {error}"),
                        });
                        app.pending_attachments = attachments;
                        return;
                    }
                }
            };
            match agent.steer_input(None, message).await {
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
                    app.queued_turns.push_back(QueuedTurn {
                        text: instruction.to_string(),
                        attachments,
                        interaction_mode: app.interaction_mode,
                    });
                    app.status_msg = format!(
                        "Current stage is not steerable; queued {} follow-up(s)",
                        app.queued_turns.len()
                    );
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
            let action = slash_cmd.unwrap_or(SlashCommand::Tasks);
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
            let result = match action {
                SlashCommand::TaskCancel => {
                    if store.cancel_run(&run_id) {
                        Ok("cancelled")
                    } else {
                        Err("run is not active or has no cancellation handle".to_string())
                    }
                }
                SlashCommand::TaskPause => store
                    .transition_run(
                        &run_id,
                        echo_agent_app_core::tasks::task_runtime::TaskRunStatus::Paused,
                    )
                    .map(|_| "paused")
                    .map_err(|error| error.to_string()),
                SlashCommand::TaskResume => store
                    .resume_task_run(&run_id)
                    .map(|_| "resumed")
                    .map_err(|error| error.to_string()),
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
        Some(SlashCommand::Test)
        | Some(SlashCommand::CodeReview)
        | Some(SlashCommand::Diff)
        | Some(SlashCommand::Git)
        | Some(SlashCommand::Pipeline)
        | Some(SlashCommand::Cron)
        | Some(SlashCommand::AutoMemory) => {
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
                SlashCommand::Cron => format!(
                    "Manage the requested scheduled task operation: {}",
                    args.trim()
                ),
                SlashCommand::AutoMemory => format!(
                    "Configure automatic memory behavior as requested: {}",
                    args.trim()
                ),
                _ => String::new(),
            };
            dispatch_turn(
                app,
                agent,
                agent_tx,
                QueuedTurn {
                    text: prompt,
                    attachments: Vec::new(),
                    interaction_mode: app.interaction_mode,
                },
            )
            .await;
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
    app.active_cancel = None;
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
    let runtime_messages = echo_agent_app_core::conversation_restore::restore_messages(&stored);
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
    app.task_runtime_view = Some(TaskRuntimeView {
        run_id: run.run_id,
        goal: run.goal,
        status: run.status.as_str().to_string(),
        tasks,
    });
}

fn format_task_runtime_view(view: &TaskRuntimeView) -> String {
    let mut content = format!("Run {} [{}]\nGoal: {}", view.run_id, view.status, view.goal);
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
        content.push_str(&format!(
            "\n  [{}] {} · {} tools{}",
            run.status, run.agent, run.tool_calls, usage
        ));
    }
}

fn parse_hook_event(name: &str) -> Option<echo_agent::skills::hooks::HookEvent> {
    use echo_agent::skills::hooks::HookEvent;
    match name {
        "PreToolUse" => Some(HookEvent::PreToolUse),
        "PostToolUse" => Some(HookEvent::PostToolUse),
        "PostToolUseFailure" => Some(HookEvent::PostToolUseFailure),
        "SessionStart" => Some(HookEvent::SessionStart),
        "SessionEnd" => Some(HookEvent::SessionEnd),
        "Stop" => Some(HookEvent::Stop),
        "UserPromptSubmit" => Some(HookEvent::UserPromptSubmit),
        "ConfigChange" => Some(HookEvent::ConfigChange),
        _ => None,
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
            ..
        } => {
            if let Some(run) = find_subagent_run_mut(app, execution_id.as_deref(), agent) {
                run.status = "completed".to_string();
                run.duration_ms = Some(*duration_ms);
                run.tokens_used = *tokens_used;
            }
        }
        SubagentEvent::DispatchFailed {
            agent,
            execution_id,
            ..
        } => {
            if let Some(run) = find_subagent_run_mut(app, execution_id.as_deref(), agent) {
                run.status = "failed".to_string();
            }
        }
        SubagentEvent::DispatchCancelled {
            agent,
            execution_id,
            ..
        } => {
            if let Some(run) = find_subagent_run_mut(app, execution_id.as_deref(), agent) {
                run.status = "cancelled".to_string();
            }
        }
        _ => {}
    }
    if app.subagent_runs.len() > 50 {
        let remove = app.subagent_runs.len().saturating_sub(50);
        app.subagent_runs.drain(..remove);
    }
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
    use super::{
        complete_file_reference, delete_previous_word, format_task_runtime_view, handle_esc,
        move_cursor_vertical, parse_interaction_mode, reverse_history_search,
        slash_command_allowed_while_busy, update_subagent_runs,
    };
    use crate::tui::{TaskRuntimeTaskView, TaskRuntimeView, Theme, TuiApp};
    use echo_agent_app_core::tasks::task_runtime::types::InteractionMode;

    fn app() -> TuiApp {
        let theme =
            Theme::from_color_theme(&echo_agent_app_core::output::theme::ColorTheme::dark());
        TuiApp::new("test-model".to_string(), "test".to_string(), theme)
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
    fn interrupt_cancels_backend_but_keeps_turn_busy_until_settle() {
        let mut app = app();
        let cancel = echo_agent::agent::CancellationToken::new();
        app.is_processing = true;
        app.active_cancel = Some(cancel.clone());

        handle_esc(&mut app);

        assert!(cancel.is_cancelled());
        assert!(app.is_processing);
        assert_eq!(app.status_msg, "Cancelling...");
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
    fn repeated_idle_escape_requests_rewind() {
        let mut app = app();

        handle_esc(&mut app);
        assert!(!app.rewind_requested);
        handle_esc(&mut app);

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
    fn task_runtime_projection_formats_plan_state() {
        let view = TaskRuntimeView {
            run_id: "run-1".to_string(),
            goal: "补齐 TUI".to_string(),
            status: "running".to_string(),
            tasks: vec![TaskRuntimeTaskView {
                title: "实现队列".to_string(),
                status: "completed".to_string(),
                agent_role: "implementer".to_string(),
            }],
        };

        let text = format_task_runtime_view(&view);
        assert!(text.contains("run-1 [running]"));
        assert!(text.contains("[completed] 实现队列 (implementer)"));
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
                message_id: None,
                background: false,
            },
        );
        update_subagent_runs(
            &mut app,
            &SubagentEvent::DispatchToolStarted {
                parent: "main".to_string(),
                agent: "explorer".to_string(),
                name: "read_file".to_string(),
                args: serde_json::json!({}),
                execution_id: Some("task-1:1".to_string()),
                run_id: Some("run-1".to_string()),
            },
        );
        update_subagent_runs(
            &mut app,
            &SubagentEvent::DispatchCompleted {
                parent: "main".to_string(),
                agent: "explorer".to_string(),
                duration_ms: 120,
                tokens_used: Some(42),
                iterations: Some(1),
                output: "done".to_string(),
                execution_id: Some("task-1:1".to_string()),
                run_id: Some("run-1".to_string()),
            },
        );

        let run = app.subagent_runs.first().cloned().unwrap_or_default();
        assert_eq!(run.status, "completed");
        assert_eq!(run.tool_calls, 1);
        assert_eq!(run.tokens_used, Some(42));
        assert_eq!(run.duration_ms, Some(120));
    }
}
