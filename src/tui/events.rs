//! TUI event loop — handles keyboard input, terminal resize, and agent streaming.

use super::{
    ChatMessage, MessageRole, SubagentRuntimeView, TaskRuntimeRequirementView, TaskRuntimeTaskView,
    TaskRuntimeView, ToolExecutionMessage, ToolExecutionStatus, TuiApp, TuiTurnRequest,
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

use echo_agent::subagent::SubagentEvent;
use echo_agent::tools::{ToolFailure, artifact::ToolOutputArtifactRef};
use echo_agent_app_core::api::chat_driver::TurnOutcome;
use echo_agent_app_core::api::context_window::ContextWindowSnapshot;
use echo_agent_app_core::api::conversation_input::{
    ConversationInputAddress, ConversationInputAttempt, ConversationInputFact,
    ConversationInputPhase, ConversationInputProjection, ConversationInputReceipt,
    ConversationInputSource, stable_scoped_input_id,
};
use echo_agent_app_core::api::foreground_turn::{
    ForegroundTurnLease, ForegroundTurnSnapshot, ForegroundTurnSurface,
};
use echo_agent_app_core::api::terminal::TerminalEvent;

/// Poll interval for non-blocking event check.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const PASTE_ATTACHMENT_CHAR_THRESHOLD: usize = 1_000;
const MAX_TUI_TERMINAL_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

/// Handle keyboard input when an approval request is pending.
/// Returns `true` if the key was consumed.
async fn handle_approval_key(
    _app: &mut TuiApp,
    pending_handle: &echo_agent_app_core::api::hitl::PendingApprovalQueue,
    key: &KeyEvent,
) -> bool {
    use echo_agent::human_loop::HumanLoopResponse;
    use echo_agent_app_core::api::hitl::PendingHumanLoopKind;

    let mut guard = match pending_handle.try_lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    echo_agent_app_core::api::hitl::prune_closed_pending(&mut guard);
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
    approval: &mut echo_agent_app_core::api::hitl::PendingApproval,
) -> Option<echo_agent::human_loop::HumanLoopResponse> {
    use echo_agent::human_loop::{ApprovalScope, HumanLoopResponse};
    use echo_agent_app_core::api::hitl::PendingHumanLoopKind;

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
    pending: &mut echo_agent_app_core::api::hitl::tui_provider::TuiHumanLoopState,
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
        artifact: Option<ToolOutputArtifactRef>,
        failure: Option<ToolFailure>,
    },
    /// A tool execution completed.
    ToolResult {
        call_id: String,
        output: String,
        success: bool,
        artifact: Option<ToolOutputArtifactRef>,
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
    Execution(echo_agent_app_core::api::tasks::task_runtime::executor::ExecEvent),
    TurnStatus(String),
    ConversationInputReceipt(Box<ConversationInputReceipt>),
    /// The sole TUI lifecycle terminal, emitted after the driver settles.
    TurnSettled {
        turn_id: String,
        outcome: TurnOutcome,
    },
    ExecutionPath {
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
    use echo_agent::tools::artifact::ToolOutputArtifactRef;
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
            artifact: None,
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
            tool_command(
                "apply_patch",
                r#"{"patch":"*** Begin Patch\n*** Update File: src/lib.rs\n*** End Patch"}"#
            ),
            "Apply patch"
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
    fn typed_artifact_is_visible_without_marking_tool_failed() {
        let mut tool = execution(
            "shell",
            r#"{"command":"large-output"}"#,
            ToolExecutionStatus::Succeeded,
        );
        tool.truncated = true;
        tool.artifact = Some(ToolOutputArtifactRef {
            path: "/tool-output/tool.log".into(),
            artifact_bytes: 1_048_576,
            payload_bytes: 1_048_576,
            sha256: "typed-artifact".to_string(),
            retention: "conversation_or_30d".to_string(),
        });

        assert!(tool_metadata_label(&tool).contains("artifact 1.0 MiB"));
        assert!(
            tool_output_tail(&tool, 6)
                .iter()
                .any(|line| line.contains("full output: /tool-output/tool.log"))
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
    let mut terminal_event_rx = app
        .app_state
        .as_ref()
        .map(|state| state.terminal.subscribe());
    let mut subagent_event_rx = agent
        .read(|a| a.subagent_registry().event_bus().subscribe())
        .await;
    let mut last_runtime_refresh = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);

    loop {
        let terminal_events_closed = terminal_event_rx
            .as_mut()
            .is_some_and(|events| drain_terminal_events(app, events));
        if terminal_events_closed {
            terminal_event_rx = None;
        }
        while let Ok(event) = subagent_event_rx.try_recv() {
            update_subagent_runs(app, &event);
        }

        if last_runtime_refresh.elapsed() >= Duration::from_millis(250) {
            refresh_task_runtime_view(app).await;
            if let Ok(address) = tui_conversation_input_address(app) {
                refresh_conversation_input_frontier(app, &address).await;
            }
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
                            artifact: None,
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
                    artifact,
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
                        if artifact.is_some() {
                            tool.artifact = artifact;
                        }
                        tool.metadata = metadata;
                    }
                    app.invalidate_messages_cache();
                }
                AgentEvent::ToolResult {
                    call_id,
                    output,
                    success,
                    artifact,
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
                        if artifact.is_some() {
                            tool.artifact = artifact;
                        }
                        if tool.name == "apply_patch" {
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
                AgentEvent::ConversationInputReceipt(receipt) => {
                    let service = app
                        .app_state
                        .as_ref()
                        .map(|state| state.conversation_inputs());
                    render_conversation_input_receipt(app, *receipt, service).await;
                }
                AgentEvent::TurnSettled { turn_id, outcome } => {
                    let settled_address = tui_conversation_input_address(app).ok();
                    if apply_turn_settlement(app, &turn_id, &outcome)
                        && let Some(address) = settled_address
                    {
                        dispatch_next_conversation_input(app, &agent, agent_tx.clone(), address)
                            .await;
                    }
                }
                AgentEvent::ExecutionPath { observed_path } => {
                    app.status_msg = observed_path;
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

        if app.conversation_input_queue_len() > 0
            && let Ok(address) = tui_conversation_input_address(app)
        {
            match authoritative_tui_foreground(app, &address) {
                Ok(None) => {
                    dispatch_next_conversation_input(app, &agent, agent_tx.clone(), address).await;
                }
                Ok(Some(_)) => {}
                Err(error) => {
                    app.status_msg = format!("Foreground projection unavailable: {error}")
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
                Ok(Event::Paste(text)) if app.active_terminal_id.is_some() => {
                    send_terminal_input(app, text.as_bytes()).await;
                }
                Ok(Event::Paste(text)) => handle_pasted_text(app, &text),
                Ok(Event::Mouse(_)) if app.active_terminal_id.is_some() => {}
                Ok(Event::Mouse(mouse)) => handle_mouse(app, &mouse),
                Ok(Event::Resize(cols, rows)) if app.active_terminal_id.is_some() => {
                    resize_active_terminal(app, rows, cols).await;
                }
                Ok(Event::Resize(_, _)) => {}
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
    app.active_turn_workspace_id = None;
    app.active_turn_conversation_id = None;
    app.active_turn_execution_root = None;
    app.active_turn_agent = None;
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
    if app.active_terminal_id.is_some() {
        handle_terminal_key(app, &key).await;
        return;
    }
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

fn append_terminal_output(app: &mut TuiApp, bytes: &[u8]) {
    app.terminal_output.extend_from_slice(bytes);
    let excess = app
        .terminal_output
        .len()
        .saturating_sub(MAX_TUI_TERMINAL_OUTPUT_BYTES);
    if excess > 0 {
        app.terminal_output.drain(..excess);
    }
}

fn drain_terminal_events(
    app: &mut TuiApp,
    events: &mut tokio::sync::broadcast::Receiver<TerminalEvent>,
) -> bool {
    loop {
        match events.try_recv() {
            Ok(TerminalEvent::Output { id, bytes })
                if app.active_terminal_id.as_deref() == Some(id.as_str()) =>
            {
                append_terminal_output(app, &bytes);
            }
            Ok(TerminalEvent::Exited { id, reason })
                if app.active_terminal_id.as_deref() == Some(id.as_str()) =>
            {
                app.active_terminal_id = None;
                app.status_msg = format!("Terminal '{id}' exited: {reason:?}");
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => return false,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                app.status_msg = format!(
                    "Terminal output lagged by {skipped} event(s); the visible tail remains live"
                );
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return true,
        }
    }
}

async fn handle_terminal_key(app: &mut TuiApp, key: &KeyEvent) {
    if key.code == KeyCode::Esc {
        let id = app.active_terminal_id.take().unwrap_or_default();
        app.status_msg = format!("Detached from terminal '{id}'");
        return;
    }
    let bytes = match key.code {
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Char(value) if key.modifiers.contains(KeyModifiers::CONTROL) => value
            .is_ascii_alphabetic()
            .then(|| vec![(value.to_ascii_lowercase() as u8).saturating_sub(b'a') + 1]),
        KeyCode::Char(value) => {
            let mut encoded = [0_u8; 4];
            let bytes = value.encode_utf8(&mut encoded).as_bytes();
            let mut result = Vec::with_capacity(bytes.len().saturating_add(1));
            if key.modifiers.contains(KeyModifiers::ALT) {
                result.push(0x1b);
            }
            result.extend_from_slice(bytes);
            Some(result)
        }
        _ => None,
    };
    if let Some(bytes) = bytes {
        send_terminal_input(app, &bytes).await;
    }
}

async fn send_terminal_input(app: &mut TuiApp, bytes: &[u8]) {
    let Some(id) = app.active_terminal_id.clone() else {
        return;
    };
    let Some(state) = app.app_state.as_ref() else {
        app.status_msg = "Terminal service is unavailable".to_string();
        return;
    };
    if let Err(error) = state.terminal.write(&id, bytes).await {
        app.status_msg = format!("Terminal '{id}' input failed: {error}");
        if !state.terminal.contains(&id) {
            app.active_terminal_id = None;
        }
    }
}

async fn resize_active_terminal(app: &mut TuiApp, rows: u16, cols: u16) {
    let Some(id) = app.active_terminal_id.clone() else {
        return;
    };
    let Some(state) = app.app_state.as_ref() else {
        return;
    };
    if let Err(error) = state.terminal.resize(&id, rows.max(1), cols.max(1)).await {
        app.status_msg = format!("Terminal '{id}' resize failed: {error}");
    }
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
        if active_tui_turn(app).is_ok() && !slash_command_allowed_while_busy(&text) {
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

    let attachments = std::mem::take(&mut app.pending_attachments);
    if let Err(error) =
        submit_tui_conversation_input(app, agent, agent_tx, text.clone(), attachments.clone()).await
    {
        restore_undispatched_turn(
            app,
            TuiTurnRequest {
                text,
                attachments,
                run_resume: None,
                input_attempt: None,
            },
            error,
        );
    }
}

fn tui_conversation_input_address(app: &TuiApp) -> Result<ConversationInputAddress, String> {
    let active = active_tui_turn(app).ok();
    let workspace_id = active
        .as_ref()
        .map(|snapshot| snapshot.workspace_id.as_str())
        .or_else(|| Some(app.workspace_execution_scope.workspace_id()))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "TUI workspace identity is unavailable".to_string())?;
    let conversation_id = active
        .as_ref()
        .map(|snapshot| snapshot.conversation_id.as_str())
        .or(app.conversation_id.as_deref())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "TUI conversation identity is unavailable".to_string())?;
    Ok(ConversationInputAddress {
        workspace_id: workspace_id.to_string(),
        conversation_id: conversation_id.to_string(),
    })
}

fn exact_active_turn_for_address(
    snapshot: Option<&ForegroundTurnSnapshot>,
    address: &ConversationInputAddress,
) -> Option<String> {
    snapshot
        .filter(|snapshot| {
            snapshot.workspace_id == address.workspace_id
                && snapshot.conversation_id == address.conversation_id
        })
        .map(|snapshot| snapshot.active_turn_id.clone())
}

fn authoritative_tui_foreground(
    app: &TuiApp,
    address: &ConversationInputAddress,
) -> Result<Option<ForegroundTurnSnapshot>, String> {
    let state = app
        .app_state
        .as_ref()
        .ok_or_else(|| "TUI application state is unavailable".to_string())?;
    Ok(state.session.foreground_turns.snapshot_scoped(
        &address.workspace_id,
        ForegroundTurnSurface::Tui,
        &address.conversation_id,
    ))
}

async fn submit_tui_conversation_input(
    app: &mut TuiApp,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
    text: String,
    attachments: Vec<echo_agent_app_core::api::attachments::AttachmentRef>,
) -> Result<(), String> {
    let app_state = app
        .app_state
        .as_ref()
        .cloned()
        .ok_or_else(|| "TUI application state is unavailable".to_string())?;
    let address = tui_conversation_input_address(app)?;
    let attachment_data = app_state
        .session
        .product_data_io
        .run("read TUI conversation input attachments", {
            let attachments = attachments.clone();
            move || echo_agent_app_core::api::attachments::attachment_refs_to_data(&attachments)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let external_id = uuid::Uuid::new_v4().to_string();
    let input_id = stable_scoped_input_id(&address, ConversationInputSource::Tui, &external_id)
        .map_err(|error| error.to_string())?;
    let _submitted = app_state
        .conversation_inputs()
        .submit(address.clone(), input_id, text, attachment_data)
        .await
        .map_err(|error| error.to_string())?;
    let retirement = app_state
        .session
        .product_data_io
        .run("retire submitted TUI attachment staging", move || {
            echo_agent_app_core::api::attachments::discard_staged_attachment_refs(&attachments)
        })
        .await;
    match retirement {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "failed to retire submitted TUI attachment staging");
        }
        Err(error) => {
            tracing::warn!(%error, "TUI attachment retirement owner was unavailable");
        }
    }
    refresh_conversation_input_frontier(app, &address).await;
    let active_snapshot = active_tui_turn(app).ok();
    if let Some(active_turn_id) = exact_active_turn_for_address(active_snapshot.as_ref(), &address)
    {
        let projection = match app_state
            .conversation_inputs()
            .dispatch_next(&address, active_turn_id)
            .await
        {
            Ok(Some(projection)) => projection,
            Ok(None) => return Ok(()),
            Err(error) => {
                app.status_msg = format!("Conversation input dispatch failed: {error}");
                refresh_conversation_input_frontier(app, &address).await;
                return Ok(());
            }
        };
        if let Err(error) = steer_conversation_input_projection(
            app,
            &app_state,
            &address,
            projection.clone(),
            agent_tx.clone(),
        )
        .await
        {
            if let Ok(attempt) = exact_conversation_input_attempt(&projection) {
                let _ = app_state
                    .conversation_inputs()
                    .deferred(attempt, error.clone())
                    .await;
            }
            app.status_msg = format!("Conversation input deferred: {error}");
            refresh_conversation_input_frontier(app, &address).await;
        }
    } else {
        dispatch_next_conversation_input(app, agent, agent_tx, address).await;
    }
    Ok(())
}

async fn refresh_conversation_input_frontier(app: &mut TuiApp, address: &ConversationInputAddress) {
    let Some(app_state) = app.app_state.as_ref() else {
        return;
    };
    let service = app_state.conversation_inputs();
    refresh_conversation_input_frontier_with_service(app, &service, address).await;
}

async fn refresh_conversation_input_frontier_with_service(
    app: &mut TuiApp,
    service: &echo_agent_app_core::api::conversation_input::ConversationInputService,
    address: &ConversationInputAddress,
) {
    match service.list(address).await {
        Ok(frontier) => app.conversation_input_frontier = Some(frontier),
        Err(error) => {
            tracing::warn!(%error, "failed to refresh TUI conversation input frontier");
            app.status_msg = format!("Conversation input projection unavailable: {error}");
        }
    }
}

async fn render_conversation_input_receipt(
    app: &mut TuiApp,
    receipt: ConversationInputReceipt,
    service: Option<echo_agent_app_core::api::conversation_input::ConversationInputService>,
) {
    let address = receipt.identity.address.clone();
    app.status_msg = format!(
        "Input {}: {:?}{}",
        receipt.identity.input_id,
        receipt.phase,
        if receipt.drained { " (drained)" } else { "" }
    );
    if let Some(service) = service {
        refresh_conversation_input_frontier_with_service(app, &service, &address).await;
    }
}

fn exact_conversation_input_attempt(
    projection: &ConversationInputProjection,
) -> Result<ConversationInputAttempt, String> {
    Ok(ConversationInputAttempt {
        identity: projection.receipt.identity.clone(),
        attempt: projection
            .receipt
            .attempt
            .ok_or_else(|| "conversation input attempt ordinal is missing".to_string())?,
        attempt_id: projection
            .receipt
            .attempt_id
            .clone()
            .ok_or_else(|| "conversation input attempt id is missing".to_string())?,
        turn_id: projection
            .receipt
            .turn_id
            .clone()
            .ok_or_else(|| "conversation input turn id is missing".to_string())?,
        observation: Default::default(),
    })
}

async fn stage_conversation_input_attachments(
    app_state: &echo_agent_app_core::api::state::AppState,
    workspace_root: std::path::PathBuf,
    attachments: Vec<echo_agent_app_core::api::types::AttachmentData>,
) -> Result<Vec<echo_agent_app_core::api::attachments::AttachmentRef>, String> {
    app_state
        .session
        .product_data_io
        .run("stage TUI conversation input attachments", move || {
            let uploads = echo_agent_app_core::api::attachments::resolve_uploads_dir(Some(
                workspace_root.as_path(),
            ));
            let saved =
                echo_agent_app_core::api::attachments::save_attachments(&attachments, &uploads)?;
            Ok::<_, echo_agent_app_core::api::attachments::AttachmentError>(
                saved
                    .iter()
                    .map(|(path, attachment)| {
                        echo_agent_app_core::api::attachments::AttachmentRef::from_saved(
                            path.clone(),
                            attachment,
                        )
                    })
                    .collect(),
            )
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[derive(Debug, PartialEq, Eq)]
enum RegisteredTuiSteerError {
    Registration(String),
    Handoff(String),
}

async fn execute_registered_tui_steer<Effect, EffectFuture, ResultValue>(
    registration: Result<(), echo_agent_app_core::api::foreground_turn::ForegroundTurnError>,
    effect: Effect,
    handoff: tokio::sync::oneshot::Sender<ResultValue>,
) -> Result<(), RegisteredTuiSteerError>
where
    Effect: FnOnce() -> EffectFuture,
    EffectFuture: std::future::Future<Output = ResultValue>,
{
    registration.map_err(|error| RegisteredTuiSteerError::Registration(error.to_string()))?;
    let result = effect().await;
    handoff.send(result).map_err(|_| {
        RegisteredTuiSteerError::Handoff(
            "tracked steer observer ended before receipt handoff".to_string(),
        )
    })
}

async fn steer_conversation_input_projection(
    app: &mut TuiApp,
    app_state: &echo_agent_app_core::api::state::AppState,
    address: &ConversationInputAddress,
    projection: ConversationInputProjection,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) -> Result<(), String> {
    let attempt = exact_conversation_input_attempt(&projection)?;
    let execution_root = app
        .active_turn_execution_root
        .clone()
        .ok_or_else(|| "TUI active execution root is unavailable".to_string())?;
    let refs = stage_conversation_input_attachments(
        app_state,
        execution_root.clone(),
        projection.payload.attachments.clone(),
    )
    .await?;
    let spill_dir = echo_agent_app_core::api::prepared_turn::resolve_user_input_spill_dir(Some(
        execution_root.as_path(),
    ));
    let prepared = echo_agent_app_core::api::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::api::prepared_turn::UserTurnInput {
            text: &projection.payload.text,
            attachments: &refs,
            spill_dir: &spill_dir,
            conversation_id: Some(&address.conversation_id),
            turn_id: Some(&attempt.turn_id),
        },
    )
    .map_err(|error| error.to_string())?;
    let message = prepared.to_message().map_err(|error| error.to_string())?;
    let agent = app
        .active_turn_agent
        .as_ref()
        .ok_or_else(|| "TUI active Agent is unavailable".to_string())?;
    let service = app_state.conversation_inputs();
    let observer_attempt = attempt.clone();
    let receipt_tx = agent_tx;
    let (steer_tx, steer_rx) = tokio::sync::oneshot::channel();
    let observer = async move {
        let result = match steer_rx.await {
            Ok(result) => result,
            Err(error) => {
                let detail = format!("tracked steer handoff failed: {error}");
                service
                    .recovery_required(observer_attempt.clone(), detail.clone())
                    .await
                    .map_err(|projection| {
                        format!("{detail}; recovery projection also failed: {projection}")
                    })?;
                return Err(detail);
            }
        };
        let receipt = service
            .observe_steer_through_drain(observer_attempt, result)
            .await
            .map_err(|error| error.to_string())?;
        receipt_tx
            .send(AgentEvent::ConversationInputReceipt(Box::new(receipt)))
            .map_err(|_| "TUI conversation input receipt receiver closed".to_string())?;
        Ok(())
    };
    let terminal_service = app_state.conversation_inputs();
    let terminal_attempt = attempt.clone();
    let terminal_projector: echo_agent_app_core::api::foreground_turn::ForegroundTerminalProjector =
        Arc::new(move |outcome| {
            let service = terminal_service.clone();
            let attempt = terminal_attempt.clone();
            Box::pin(async move {
                service
                    .settle_attempt(&attempt, &outcome)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        });
    let registration = app_state
        .session
        .foreground_turns
        .supervise_input_lifecycle_scoped(
            &address.workspace_id,
            ForegroundTurnSurface::Tui,
            &address.conversation_id,
            &attempt.turn_id,
            observer,
            terminal_projector,
        );
    match execute_registered_tui_steer(
        registration,
        || agent.steer_input_tracked(Some(&attempt.turn_id), message),
        steer_tx,
    )
    .await
    {
        Ok(()) => {
            app.status_msg = format!(
                "Input {} accepted for observation",
                projection.receipt.identity.input_id
            );
        }
        Err(RegisteredTuiSteerError::Registration(error)) => {
            let detail = format!("foreground input observer admission failed: {error}");
            app_state
                .conversation_inputs()
                .deferred(attempt, detail.clone())
                .await
                .map_err(|settlement| settlement.to_string())?;
            app.status_msg = detail;
        }
        Err(RegisteredTuiSteerError::Handoff(detail)) => {
            app_state
                .conversation_inputs()
                .recovery_required(attempt, detail.clone())
                .await
                .map_err(|settlement| settlement.to_string())?;
            app.status_msg = detail;
        }
    }
    refresh_conversation_input_frontier(app, address).await;
    Ok(())
}

enum TurnDispatchResult {
    Started,
    Rejected {
        turn: Box<TuiTurnRequest>,
        error: String,
    },
}

async fn settle_rejected_tui_turn(
    lease: ForegroundTurnLease,
    code: &'static str,
    detail: String,
) -> String {
    let requested = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
        code,
        detail.clone(),
    ));
    match lease.settle_after_observers(requested).await {
        Ok(settlement) => match settlement.outcome {
            TurnOutcome::Failed(failure) if failure.code != code => {
                format!("{detail}; terminal [{}]: {}", failure.code, failure.message)
            }
            _ => detail,
        },
        Err(error) => format!("{detail}; foreground settlement debt: {error}"),
    }
}

async fn dispatch_turn(
    app: &mut TuiApp,
    _agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
    turn: TuiTurnRequest,
) -> TurnDispatchResult {
    let turn_id = turn
        .input_attempt
        .as_ref()
        .map(|attempt| attempt.turn_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let run_turn_binding = run_turn_binding_for_request(&turn, &turn_id);
    let planned_resume = turn
        .run_resume
        .as_ref()
        .filter(|resume| !resume.is_continuation)
        .map(|resume| resume.identity.clone());
    let app_state = match app.app_state.as_ref() {
        Some(state) => state.clone(),
        None => {
            return TurnDispatchResult::Rejected {
                turn: Box::new(turn),
                error: "TUI application state is unavailable".to_string(),
            };
        }
    };
    let foreground_turns = app_state.session.foreground_turns.clone();
    let (scoped_runtime, conversation_id, lease) =
        match begin_tui_foreground_turn(app, &turn_id).await {
            Ok(admission) => admission,
            Err(error) => {
                tracing::warn!(%error, turn_id, "failed to acquire TUI foreground turn");
                return TurnDispatchResult::Rejected {
                    turn: Box::new(turn),
                    error: format!("Unable to start foreground turn: {error}"),
                };
            }
        };
    if let Some(resume) = turn.run_resume.as_ref() {
        let identity = &resume.identity;
        let validation = if identity.workspace_id != scoped_runtime.execution_scope().workspace_id()
        {
            Err(format!(
                "TaskRun '{}' was queued for workspace '{}', but current workspace is '{}'",
                identity.run_id,
                identity.workspace_id,
                scoped_runtime.execution_scope().workspace_id()
            ))
        } else if identity.conversation_id != conversation_id {
            Err(format!(
                "TaskRun '{}' was queued for conversation '{}', but current conversation is '{}'",
                identity.run_id, identity.conversation_id, conversation_id
            ))
        } else {
            Ok(())
        };
        if let Err(detail) = validation {
            let detail = settle_rejected_tui_turn(lease, "task_run_resume", detail).await;
            return TurnDispatchResult::Rejected {
                turn: Box::new(turn),
                error: detail,
            };
        }
    }
    if let Some(attempt) = turn.input_attempt.as_ref() {
        let address = &attempt.identity.address;
        if address.workspace_id != scoped_runtime.execution_scope().workspace_id()
            || address.conversation_id != conversation_id
        {
            let detail = format!(
                "Conversation input {} targets {}/{}, but TUI dispatch resolved {}/{}",
                attempt.identity.input_id,
                address.workspace_id,
                address.conversation_id,
                scoped_runtime.execution_scope().workspace_id(),
                conversation_id
            );
            let detail = settle_rejected_tui_turn(lease, "conversation_input", detail).await;
            return TurnDispatchResult::Rejected {
                turn: Box::new(turn),
                error: detail,
            };
        }
    }
    let pool_execution = match scoped_runtime.agent_for(&conversation_id).await {
        Ok(execution) => execution,
        Err(error) => {
            let detail = format!("TUI AgentPool admission failed: {error}");
            let detail = settle_rejected_tui_turn(lease, "agent_pool", detail).await;
            return TurnDispatchResult::Rejected {
                turn: Box::new(turn),
                error: detail,
            };
        }
    };
    {
        let title: String = turn.text.chars().take(80).collect();
        if let Err(error) = scoped_runtime
            .ensure_conversation(echo_agent::memory::NewConversation {
                conversation_id: conversation_id.clone(),
                user_id: "default".to_string(),
                agent_type: None,
                title: Some(title),
            })
            .await
        {
            tracing::warn!(error = %error, conversation_id, "failed to ensure TUI conversation metadata");
            let detail = format!("Unable to persist TUI conversation metadata: {error}");
            let detail = settle_rejected_tui_turn(lease, "conversation_store", detail).await;
            return TurnDispatchResult::Rejected {
                turn: Box::new(turn),
                error: detail,
            };
        }
    }
    let renderer: std::sync::Arc<dyn echo_agent_app_core::api::chat_driver::ChatSink> =
        std::sync::Arc::new(TuiChatSink::new(agent_tx.clone()));
    let sink = echo_agent_app_core::api::chat_event_log::bind_surface_chat_sink(
        echo_agent_app_core::api::chat_event_log::ChatSurface::Tui,
        renderer,
        app_state.storage.chat_events.clone(),
        app_state.storage.tool_executions.clone(),
        scoped_runtime.execution_scope().workspace_id().to_string(),
        Some(conversation_id.clone()),
        turn_id.clone(),
    );
    let spill_dir = echo_agent_app_core::api::prepared_turn::resolve_user_input_spill_dir(Some(
        scoped_runtime.execution_scope().root(),
    ));
    let prepared = match echo_agent_app_core::api::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::api::prepared_turn::UserTurnInput {
            text: &turn.text,
            attachments: &turn.attachments,
            spill_dir: &spill_dir,
            conversation_id: Some(&conversation_id),
            turn_id: Some(&turn_id),
        },
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::warn!(%error, "failed to prepare TUI user turn");
            let detail = format!("Failed to prepare user turn: {error}");
            let detail = settle_rejected_tui_turn(lease, "prepared_turn", detail).await;
            return TurnDispatchResult::Rejected {
                turn: Box::new(turn),
                error: detail,
            };
        }
    };
    let retry_turn = request_from_prepared(&turn, &prepared);
    let task_attachments = prepared.inline_attachment_refs();
    let display_text = if turn.text.is_empty() {
        format!("[{} attachment(s)]", turn.attachments.len())
    } else {
        turn.text.clone()
    };
    let res = std::sync::Arc::new(echo_agent_app_core::api::chat_resources::ChatResources {
        execution_scope: scoped_runtime.execution_scope().clone(),
        workspace_io_receipt: Some(scoped_runtime.workspace_io_receipt()),
        pool: scoped_runtime.pool(),
        store: scoped_runtime.task_runtime(),
        sink,
        webhook_emitter: app.webhook_emitter.clone(),
        // TUI/GUI parity (AGENTS.md): bind this turn to the session's
        // conversation id so TaskRuntime runs + transcript projection work.
        conv_id: Some(conversation_id.clone()),
        root_message_id: turn_id.clone(),
        // Bind staged refs so subagents in an autonomous run see them too.
        attachments: task_attachments,
        cancel: lease.cancellation_token(),
        review_integration: scoped_runtime.review_integration(),
        memory_generation: None,
        human_loop_provider: app.human_loop_provider.clone().map(|provider| {
            provider as std::sync::Arc<dyn echo_agent::human_loop::HumanLoopProvider>
        }),
    });
    let agent_owned = pool_execution.agent();
    let active_turn_agent = agent_owned.clone();
    let active_turn_workspace_id = scoped_runtime.execution_scope().workspace_id().to_string();
    let active_turn_conversation_id = conversation_id.clone();
    let active_turn_execution_root = scoped_runtime.execution_scope().root().to_path_buf();
    let scoped_runtime_guard = scoped_runtime.clone();
    let settled_turn_id = turn_id.clone();
    let input_observation = turn
        .input_attempt
        .clone()
        .map(|attempt| (app_state.conversation_inputs(), attempt));
    if let Err(error) = foreground_turns.supervise(lease, move |lease| async move {
        let _scoped_runtime_guard = scoped_runtime_guard;
        let outcome = match std::panic::AssertUnwindSafe(send_to_agent(
            &agent_owned,
            prepared,
            res,
            TuiAgentTurnContext {
                lease,
                run_turn_binding,
                planned_resume,
                pool_execution,
                input_observation,
                receipt_tx: agent_tx.clone(),
            },
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
            turn: Box::new(retry_turn),
            error: format!("Unable to supervise foreground turn: {error}"),
        };
    }
    app.start_turn(&display_text);
    app.active_turn_id = Some(turn_id);
    app.active_turn_workspace_id = Some(active_turn_workspace_id);
    app.active_turn_conversation_id = Some(active_turn_conversation_id);
    app.active_turn_execution_root = Some(active_turn_execution_root);
    app.active_turn_agent = Some(active_turn_agent);
    TurnDispatchResult::Started
}

fn request_from_prepared(
    turn: &TuiTurnRequest,
    prepared: &echo_agent_app_core::api::prepared_turn::PreparedUserTurn,
) -> TuiTurnRequest {
    TuiTurnRequest {
        text: prepared.instruction.clone(),
        attachments: prepared.inline_attachment_refs(),
        run_resume: turn.run_resume.clone(),
        input_attempt: turn.input_attempt.clone(),
    }
}

async fn begin_tui_foreground_turn(
    app: &TuiApp,
    turn_id: &str,
) -> Result<
    (
        echo_agent_app_core::api::state::ScopedChatRuntime,
        String,
        ForegroundTurnLease,
    ),
    echo_agent_app_core::api::state::ScopedChatTurnError,
> {
    let app_state = app.app_state.as_ref().ok_or_else(|| {
        echo_agent_app_core::api::state::ScopedChatTurnError::Runtime(
            "TUI application state is unavailable".to_string(),
        )
    })?;
    let runtime = app_state
        .current_control_runtime()
        .await
        .map_err(echo_agent_app_core::api::state::ScopedChatTurnError::Control)?;
    let conversation_id = runtime
        .primary_agent()
        .read(|agent| agent.conversation_id().map(str::to_string))
        .await
        .filter(|conversation_id| !conversation_id.trim().is_empty())
        .ok_or({
            echo_agent_app_core::api::state::ScopedChatTurnError::Conversation(
                echo_agent_app_core::api::conversation_deletion::ConversationDeletionError::Foreground(
                    echo_agent_app_core::api::foreground_turn::ForegroundTurnError::EmptyConversationId,
                ),
            )
        })?;
    let lease = runtime
        .begin_turn(
            &app_state.session.foreground_turns,
            ForegroundTurnSurface::Tui,
            &conversation_id,
            turn_id,
        )
        .await?;
    Ok((runtime, conversation_id, lease))
}

fn restore_undispatched_turn(app: &mut TuiApp, turn: TuiTurnRequest, error: String) {
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
        .active_turn_conversation_id
        .as_deref()
        .ok_or_else(|| "TUI conversation id is unavailable".to_string())?;
    let expected_turn_id = app
        .active_turn_id
        .as_deref()
        .ok_or_else(|| "TUI turn projection is unavailable".to_string())?;
    let workspace_id = app
        .active_turn_workspace_id
        .as_deref()
        .ok_or_else(|| "TUI workspace projection is unavailable".to_string())?;
    let snapshot = app_state
        .session
        .foreground_turns
        .snapshot_scoped(workspace_id, ForegroundTurnSurface::Tui, conversation_id)
        .ok_or_else(|| "No active TUI foreground turn".to_string())?;
    if snapshot.root_turn_id != expected_turn_id {
        return Err(format!(
            "TUI foreground root mismatch: expected {expected_turn_id}, actual {}",
            snapshot.root_turn_id
        ));
    }
    Ok(snapshot)
}

async fn dispatch_next_conversation_input(
    app: &mut TuiApp,
    agent: &AgentHandle,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
    address: ConversationInputAddress,
) {
    let Some(app_state) = app.app_state.as_ref().cloned() else {
        return;
    };
    let service = app_state.conversation_inputs();
    let turn_id = uuid::Uuid::new_v4().to_string();
    let projection = match service.dispatch_next(&address, turn_id).await {
        Ok(projection) => projection,
        Err(error) => {
            app.status_msg = format!("Conversation input dispatch failed: {error}");
            refresh_conversation_input_frontier(app, &address).await;
            return;
        }
    };
    let Some(projection) = projection else {
        refresh_conversation_input_frontier(app, &address).await;
        return;
    };
    let attempt = match exact_conversation_input_attempt(&projection) {
        Ok(attempt) => attempt,
        Err(error) => {
            app.status_msg = error;
            refresh_conversation_input_frontier(app, &address).await;
            return;
        }
    };
    let attachments = match stage_conversation_input_attachments(
        &app_state,
        app.workspace_execution_scope.root().to_path_buf(),
        projection.payload.attachments.clone(),
    )
    .await
    {
        Ok(attachments) => attachments,
        Err(error) => {
            let _ = service.deferred(attempt, error.clone()).await;
            app.status_msg = format!("Conversation input staging deferred: {error}");
            refresh_conversation_input_frontier(app, &address).await;
            return;
        }
    };
    let result = dispatch_turn(
        app,
        agent,
        agent_tx,
        TuiTurnRequest {
            text: projection.payload.text,
            attachments,
            run_resume: None,
            input_attempt: Some(attempt.clone()),
        },
    )
    .await;
    if let TurnDispatchResult::Rejected { turn, error, .. } = result {
        if let Err(settlement_error) = service.deferred(attempt, error.clone()).await {
            tracing::warn!(%settlement_error, "failed to defer rejected TUI conversation input");
        }
        let attachments = turn.attachments;
        let cleanup = app_state
            .session
            .product_data_io
            .run("clean rejected TUI conversation input staging", move || {
                echo_agent_app_core::api::attachments::discard_staged_attachment_refs(&attachments)
            })
            .await;
        if !matches!(cleanup, Ok(Ok(()))) {
            tracing::warn!(?cleanup, "failed to clean rejected TUI input staging");
        }
        app.status_msg = format!("Conversation input is waiting for admission: {error}");
        refresh_conversation_input_frontier(app, &address).await;
    }
}

fn format_conversation_input_fact(fact: &ConversationInputFact) -> String {
    let phase = match fact {
        ConversationInputFact::Persisted { .. } => ConversationInputPhase::Persisted,
        ConversationInputFact::AttemptStarted { .. } => ConversationInputPhase::AttemptStarted,
        ConversationInputFact::MailboxAccepted { .. } => ConversationInputPhase::MailboxAccepted,
        ConversationInputFact::Drained { .. } => ConversationInputPhase::Drained,
        ConversationInputFact::TurnSettled { .. } => ConversationInputPhase::TurnSettled,
        ConversationInputFact::Deferred { .. } => ConversationInputPhase::Deferred,
        ConversationInputFact::RecoveryRequired { .. } => ConversationInputPhase::RecoveryRequired,
        ConversationInputFact::Reordered { .. } => ConversationInputPhase::Persisted,
        ConversationInputFact::Cancelled { .. } => ConversationInputPhase::Cancelled,
    };
    if matches!(fact, ConversationInputFact::Reordered { .. }) {
        return format!("Input order updated around {}", fact.identity().input_id);
    }
    format!("Input {}: {phase:?}", fact.identity().input_id)
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
    use echo_agent_app_core::api::types::{AttachmentData, AttachmentSource};

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
    match echo_agent_app_core::api::attachments::stage_attachment_data(
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
        .root_cancel_and_wait_scoped(
            &snapshot.workspace_id,
            ForegroundTurnSurface::Tui,
            &snapshot.conversation_id,
            &snapshot.root_turn_id,
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

impl echo_agent_app_core::api::chat_driver::ChatSink for TuiChatSink {
    fn on_event(&self, event: echo_agent_app_core::api::chat_driver::ChatDriverEvent) -> bool {
        use echo_agent_app_core::api::chat_driver::ChatDriverEvent;

        if let Some(projection) =
            echo_agent_app_core::api::tasks::task_runtime::project_command_cell_watch_surface_event(
                &event,
            )
        {
            return self
                .tx
                .send(AgentEvent::Notice(projection.display_message()))
                .is_ok();
        }
        let mapped = match event {
            ChatDriverEvent::Execution(event) => AgentEvent::Execution(event),
            ChatDriverEvent::TurnStatus { status } => AgentEvent::TurnStatus(status),
            ChatDriverEvent::InputLifecycle(fact) => {
                AgentEvent::Notice(format_conversation_input_fact(&fact))
            }
            ChatDriverEvent::ExecutionPath { observed_path } => {
                AgentEvent::ExecutionPath { observed_path }
            }
            ChatDriverEvent::TurnConfiguration {
                permission_mode,
                approval_policy,
                attachments,
            } => AgentEvent::Notice(format!(
                "Turn configuration: permission={permission_mode}, approval={approval_policy}, \
                 attachments={}",
                attachments.len()
            )),
            ChatDriverEvent::Interrupt {
                run_id,
                goal,
                new_message,
            } => AgentEvent::Interrupt {
                run_id,
                goal,
                new_message,
            },
            ChatDriverEvent::ApprovalRequest {
                request_id,
                tool_name,
                prompt,
                ..
            } => AgentEvent::Notice(format!(
                "Approval requested [{request_id}] for {tool_name}: {prompt}"
            )),
            ChatDriverEvent::InputRequest { request_id, prompt } => {
                AgentEvent::Notice(format!("Input requested [{request_id}]: {prompt}"))
            }
            ChatDriverEvent::SelectionRequest {
                request_id,
                prompt,
                options,
                ..
            } => AgentEvent::Notice(format!(
                "Selection requested [{request_id}]: {prompt} ({})",
                options.join(", ")
            )),
            ChatDriverEvent::CommandCellStarted { cell } => AgentEvent::Notice(format!(
                "Command cell {} started: {}",
                cell.cell_id, cell.name
            )),
            ChatDriverEvent::CommandCellSettled { cell } => AgentEvent::Notice(format!(
                "Command cell {} settled: {}",
                cell.cell_id, cell.phase
            )),
            ChatDriverEvent::CommandCellWatchReady { .. }
            | ChatDriverEvent::CommandCellWatchDeliveryStarted { .. }
            | ChatDriverEvent::CommandCellWatchAcknowledged { .. } => {
                AgentEvent::Notice("CommandCellWatch projection unavailable".to_string())
            }
            ChatDriverEvent::ExtensionReceipt(receipt) => {
                AgentEvent::Notice(receipt.display_message())
            }
            ChatDriverEvent::ContextCompressed {
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
                    invocation,
                } => AgentEvent::ToolCall {
                    call_id,
                    name: invocation.name,
                    args: invocation.args.to_string(),
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
                    artifact: result.artifact,
                    failure: result.failure,
                },
                echo_agent::agent::AgentEvent::ToolResult {
                    call_id, result, ..
                } => AgentEvent::ToolResult {
                    call_id,
                    output: result.error.unwrap_or(result.output),
                    success: result.success,
                    artifact: result.artifact,
                    failure: result.failure,
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

struct TuiAgentTurnContext {
    lease: ForegroundTurnLease,
    run_turn_binding: Option<echo_agent_app_core::api::tasks::task_runtime::RunTurnBinding>,
    planned_resume: Option<echo_agent_app_core::api::tasks::task_runtime::TaskRunResumeIdentity>,
    pool_execution: echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease,
    input_observation: Option<(
        echo_agent_app_core::api::conversation_input::ConversationInputService,
        ConversationInputAttempt,
    )>,
    receipt_tx: mpsc::UnboundedSender<AgentEvent>,
}

async fn settle_planned_resume_foreground(
    lease: ForegroundTurnLease,
    outcome: TurnOutcome,
) -> TurnOutcome {
    match lease.settle_after_observers(outcome).await {
        Ok(settlement) => settlement.outcome,
        Err(error) => TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
            "planned_resume_settlement",
            error.to_string(),
        )),
    }
}

async fn send_to_agent(
    agent: &AgentHandle,
    turn: echo_agent_app_core::api::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<echo_agent_app_core::api::chat_resources::ChatResources>,
    context: TuiAgentTurnContext,
) -> TurnOutcome {
    use echo_agent_app_core::api::foreground_turn::{
        drive_foreground_chat, drive_foreground_chat_turn, drive_foreground_chat_with_ingress,
    };
    let TuiAgentTurnContext {
        lease,
        run_turn_binding,
        planned_resume,
        pool_execution,
        input_observation,
        receipt_tx,
    } = context;

    // TUI does not classify chat versus task locally. The shared foreground
    // driver owns TaskRuntime, memory-generation, and pool admission, while
    // PreparedUserTurn preserves the same staged attachment path as GUI.
    if let Some(expected) = planned_resume {
        let trace_sink = echo_agent_app_core::api::chat_driver::subagent_trace_sink_for(&res.sink);
        let result = match res.store.clone() {
            Some(store) => {
                echo_agent_app_core::api::tasks::task_runtime::launch_planned_run_resume(
                    store,
                    expected,
                    agent.clone(),
                    Some(pool_execution),
                    res.review_integration.clone(),
                    Some(trace_sink),
                    lease.cancellation_token(),
                    res.workspace_io_receipt
                        .as_ref()
                        .map(|receipt| receipt.invocation()),
                )
                .await
                .map_err(|error| error.to_string())
            }
            None => Err("TaskRuntime store is unavailable".to_string()),
        };
        let outcome = match result {
            Ok(launch) => match launch.wait().await {
                Ok(echo_agent_app_core::api::tasks::task_runtime::RunOutcome::Completed) => {
                    TurnOutcome::Completed
                }
                Ok(echo_agent_app_core::api::tasks::task_runtime::RunOutcome::Cancelled) => {
                    TurnOutcome::Cancelled
                }
                Ok(other) => TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "planned_resume",
                    format!("planned resume ended with {other:?}"),
                )),
                Err(error) => TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "planned_resume",
                    error,
                )),
            },
            Err(error) => TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                "planned_resume",
                error,
            )),
        };
        return settle_planned_resume_foreground(lease, outcome).await;
    }
    let _pool_execution = pool_execution;
    let result = if let Some(binding) = run_turn_binding {
        drive_foreground_chat_turn(lease, agent, &turn, res, binding).await
    } else if let Some((service, attempt)) = input_observation {
        let observer_service = service.clone();
        let observer_attempt = attempt.clone();
        let observer: echo_agent_app_core::api::chat_driver::InputReceiptObserver =
            Arc::new(move |receipt| {
                let service = observer_service.clone();
                let attempt = observer_attempt.clone();
                let receipt_tx = receipt_tx.clone();
                Box::pin(async move {
                    let observed = service
                        .observe_turn_input_through_drain(attempt, receipt)
                        .await
                        .map_err(|error| error.to_string())?;
                    receipt_tx
                        .send(AgentEvent::ConversationInputReceipt(Box::new(observed)))
                        .map_err(|_| "TUI initial input receipt receiver closed".to_string())?;
                    Ok(())
                })
            });
        let terminal_attempt = attempt.clone();
        drive_foreground_chat_with_ingress(lease, agent, &turn, res, observer, move |outcome| {
            let service = service.clone();
            let attempt = terminal_attempt.clone();
            async move {
                service
                    .settle_attempt(&attempt, &outcome)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
        })
        .await
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

fn run_turn_binding_for_request(
    turn: &TuiTurnRequest,
    turn_id: &str,
) -> Option<echo_agent_app_core::api::tasks::task_runtime::RunTurnBinding> {
    turn.run_resume.as_ref().and_then(|resume| {
        if !resume.is_continuation {
            return None;
        }
        let identity = &resume.identity;
        Some(
            echo_agent_app_core::api::tasks::task_runtime::RunTurnBinding::resume_expected(
                identity.clone(),
                turn_id.to_string(),
            ),
        )
    })
}

async fn handle_tui_cron(app: &TuiApp, args: &str) -> String {
    use echo_agent_app_core::api::scheduler::{CronTask, CronTaskStatus};

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
    use echo_agent_app_core::api::tasks::task_runtime::worktree::{
        cleanup_unattended_worktrees, discard_unattended_worktree, git_repo_root,
        list_unattended_worktrees, merge_unattended_worktree, repo_merge_lock,
    };

    let tokens = match shell_words::split(args) {
        Ok(tokens) => tokens,
        Err(error) => return format!("Invalid worktree command: {error}"),
    };
    let subcommand = tokens.first().map(String::as_str).unwrap_or("list");
    let run_id = tokens.get(1).cloned();
    let (runtime, store) = match current_tui_task_runtime(app).await {
        Ok(control) => control,
        Err(error) => return format!("Task runtime unavailable: {error}"),
    };
    let repo_root = match git_repo_root(runtime.execution_scope().root()) {
        Ok(path) => path,
        Err(error) => return format!("Current workspace is not a Git repository: {error}"),
    };

    match subcommand {
        "list" | "ls" => {
            let operation_store = store.clone();
            let result = echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeOperation::new(
                store,
            )
            .run_owned("list TUI unattended worktrees", move || {
                list_unattended_worktrees(&repo_root, Some(operation_store.as_ref())).map_err(
                    |error| {
                        echo_agent_app_core::api::tasks::task_runtime::StoreError::InvalidPlan(
                            error.to_string(),
                        )
                    },
                )
            })
            .await;
            match result {
                Ok(worktrees) => format_unattended_worktrees(&worktrees),
                Err(error) => format!("Failed to list retained worktrees: {error}"),
            }
        }
        "cleanup" | "clean" => {
            let lock = repo_merge_lock(&repo_root);
            let _guard = lock.lock().await;
            let operation_store = store.clone();
            let result =
                echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeOperation::new(store)
                    .run_owned("clean TUI unattended worktrees", move || {
                        cleanup_unattended_worktrees(&repo_root, Some(operation_store.as_ref()))
                        .map_err(|error| {
                            echo_agent_app_core::api::tasks::task_runtime::StoreError::InvalidPlan(
                                error.to_string(),
                            )
                        })
                    })
                    .await;
            match result {
                Ok(result) => format!(
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
                Err(error) => format!("Failed to clean retained worktrees: {error}"),
            }
        }
        "merge" | "integrate" => {
            let Some(run_id) = run_id else {
                return "Usage: /worktrees merge <run-id>".to_string();
            };
            let lock = repo_merge_lock(&repo_root);
            let _guard = lock.lock().await;
            let run_id_for_merge = run_id.clone();
            let operation_store = store.clone();
            let result =
                echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeOperation::new(store)
                    .run_owned("merge TUI unattended worktree", move || {
                        merge_unattended_worktree(
                            &repo_root,
                            &run_id_for_merge,
                            Some(operation_store.as_ref()),
                        )
                        .map_err(|error| {
                            echo_agent_app_core::api::tasks::task_runtime::StoreError::InvalidPlan(
                                error.to_string(),
                            )
                        })
                    })
                    .await;
            match result {
                Ok(outcome) => {
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
                Err(error) => format!("Failed to merge retained worktree: {error}"),
            }
        }
        "discard" | "remove" | "rm" => {
            let Some(run_id) = run_id else {
                return "Usage: /worktrees discard <run-id>".to_string();
            };
            let lock = repo_merge_lock(&repo_root);
            let _guard = lock.lock().await;
            let run_id_for_discard = run_id.clone();
            let operation_store = store.clone();
            let result =
                echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeOperation::new(store)
                    .run_owned("discard TUI unattended worktree", move || {
                        discard_unattended_worktree(
                            &repo_root,
                            &run_id_for_discard,
                            Some(operation_store.as_ref()),
                        )
                        .map_err(|error| {
                            echo_agent_app_core::api::tasks::task_runtime::StoreError::InvalidPlan(
                                error.to_string(),
                            )
                        })
                    })
                    .await;
            match result {
                Ok(()) => format!("Discarded retained worktree for run {run_id}."),
                Err(error) => format!("Failed to discard retained worktree: {error}"),
            }
        }
        _ => "Usage: /worktrees [list|cleanup|merge <run-id>|discard <run-id>]".to_string(),
    }
}

fn format_unattended_worktrees(
    worktrees: &[echo_agent_app_core::api::tasks::task_runtime::worktree::UnattendedWorktreeInfo],
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
    runner: &echo_agent_app_core::api::scheduler::SchedulerRunner,
    prefix: Option<&String>,
    operation: &str,
) -> String {
    use echo_agent_app_core::api::scheduler::CronTaskStatus;

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
    out: &mut Vec<echo_agent_app_core::api::attachments::AttachmentRef>,
    path: &std::path::Path,
    workspace_root: Option<&std::path::Path>,
) -> std::io::Result<(String, String)> {
    use echo_agent_app_core::api::attachments::stage_local_attachment;
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

async fn current_tui_control_runtime(
    app: &TuiApp,
) -> Result<echo_agent_app_core::api::state::ScopedChatRuntime, String> {
    let state = app
        .app_state
        .as_ref()
        .ok_or_else(|| "TUI application state is unavailable".to_string())?;
    state
        .current_control_runtime()
        .await
        .map_err(|error| error.to_string())
}

async fn current_tui_task_runtime(
    app: &TuiApp,
) -> Result<
    (
        echo_agent_app_core::api::state::ScopedChatRuntime,
        Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
    ),
    String,
> {
    let runtime = current_tui_control_runtime(app).await?;
    let workspace_id = runtime.execution_scope().workspace_id();
    let store = runtime
        .task_runtime()
        .ok_or_else(|| format!("Task runtime is unavailable for workspace '{workspace_id}'"))?;
    if store.active_workspace_id() != workspace_id {
        return Err(format!(
            "TaskRuntime scope mismatch: current workspace '{workspace_id}', store owns '{}'",
            store.active_workspace_id()
        ));
    }
    Ok((runtime, store))
}

async fn current_tui_runtime_conversation_id(
    runtime: &echo_agent_app_core::api::state::ScopedChatRuntime,
) -> Result<String, String> {
    runtime
        .primary_agent()
        .read(|agent| agent.conversation_id().map(str::to_string))
        .await
        .filter(|conversation_id| !conversation_id.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "workspace '{}' conversation identity is unavailable",
                runtime.execution_scope().workspace_id()
            )
        })
}

async fn tui_task_runtime_io<T, F>(
    store: Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
    operation: &'static str,
    function: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(
            Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
        ) -> Result<T, echo_agent_app_core::api::tasks::task_runtime::StoreError>
        + Send
        + 'static,
{
    echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeOperation::new(store)
        .run_store(operation, function)
        .await
        .map_err(|error| error.to_string())
}

async fn resolve_tui_task_run(
    app: &TuiApp,
    runtime: &echo_agent_app_core::api::state::ScopedChatRuntime,
    store: &Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
    requested_run_id: Option<&str>,
) -> Result<echo_agent_app_core::api::tasks::task_runtime::TaskRun, String> {
    let workspace_id = runtime.execution_scope().workspace_id();
    let conversation_id = current_tui_runtime_conversation_id(runtime).await?;
    let implicit_view = requested_run_id
        .is_none()
        .then_some(app.task_runtime_view.as_ref())
        .flatten();
    let run_id = match requested_run_id.filter(|run_id| !run_id.trim().is_empty()) {
        Some(run_id) => run_id.to_string(),
        None => implicit_view
            .map(|view| view.run_id.clone())
            .ok_or_else(|| "No active task run. Supply a run id explicitly.".to_string())?,
    };
    let lookup_run_id = run_id.clone();
    let run = tui_task_runtime_io(store.clone(), "resolve TUI TaskRun", move |store| {
        store.get_run(&lookup_run_id)
    })
    .await?
    .ok_or_else(|| format!("TaskRun {run_id} was not found"))?;
    validate_tui_task_run_scope(&run, workspace_id, &conversation_id, implicit_view)?;
    Ok(run)
}

fn validate_tui_task_run_scope(
    run: &echo_agent_app_core::api::tasks::task_runtime::TaskRun,
    workspace_id: &str,
    conversation_id: &str,
    implicit_view: Option<&TaskRuntimeView>,
) -> Result<(), String> {
    if run.workspace_id != workspace_id || run.conversation_id != conversation_id {
        return Err(format!(
            "TaskRun '{}' belongs to workspace '{}' conversation '{}', not current workspace '{}' conversation '{}'",
            run.run_id, run.workspace_id, run.conversation_id, workspace_id, conversation_id
        ));
    }
    if let Some(view) = implicit_view
        && (view.workspace_id != workspace_id
            || view.conversation_id != conversation_id
            || view.run_created_at != run.created_at)
    {
        return Err(format!(
            "TaskRuntime view for '{}' is stale after a workspace or run generation change",
            run.run_id
        ));
    }
    Ok(())
}

async fn current_tui_memory_control(
    app: &TuiApp,
) -> Result<
    (
        echo_agent_app_core::api::state::ScopedChatRuntime,
        echo_agent_app_core::api::evolution::ReviewGenerationLease,
        Arc<echo_agent::evolution::MemoryLayerManager>,
    ),
    String,
> {
    let runtime = current_tui_control_runtime(app).await?;
    let workspace_id = runtime.execution_scope().workspace_id();
    let integration = runtime.review_integration().ok_or_else(|| {
        format!("Layered memory is not configured for workspace '{workspace_id}'")
    })?;
    let generation = integration
        .lease_generation()
        .map_err(|error| error.to_string())?;
    let layer_manager = generation
        .layer_manager()
        .map_err(|error| error.to_string())?;
    Ok((runtime, generation, layer_manager))
}

fn render_tui_reflection_receipt(
    receipt: &echo_agent_app_core::api::reflection::ReflectionReceipt,
) -> String {
    receipt.display_message()
}

#[cfg(test)]
mod reflection_adapter_tests {
    #[test]
    fn tui_projects_the_shared_reflection_receipt() {
        let receipt = echo_agent_app_core::api::reflection::reflection_receipt_fixture();
        let rendered = super::render_tui_reflection_receipt(&receipt);
        assert!(rendered.contains(&receipt.key));
        assert!(rendered.contains(&receipt.content_summary));
    }
}

async fn refresh_workspace_generation(
    app: &mut TuiApp,
    state: &echo_agent_app_core::api::state::AppState,
) {
    if let Err(error) = app.discard_unsubmitted_attachments() {
        tracing::warn!(%error, "failed to clean staged attachments after workspace change");
    }
    app.task_runtime_view = None;
    app.subagent_runs.clear();
    let runtime = state.current_control_runtime().await.ok();
    let fallback_scope = state.current_execution_scope().await;
    app.workspace_root = runtime
        .as_ref()
        .map(|runtime| runtime.execution_scope().root().to_path_buf());
    app.workspace_execution_scope = runtime
        .as_ref()
        .map(|runtime| runtime.execution_scope().clone())
        .unwrap_or(fallback_scope);
    app.conversation_store = runtime
        .as_ref()
        .and_then(echo_agent_app_core::api::state::ScopedChatRuntime::conversation_store);
    app.conversation_id = match runtime {
        Some(runtime) => {
            runtime
                .primary_agent()
                .read(|agent| agent.conversation_id().map(str::to_string))
                .await
        }
        None => None,
    };
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

fn parse_tui_llm_protocol(value: &str) -> Option<echo_agent::llm::LlmApiProtocol> {
    match value.trim().to_ascii_lowercase().as_str() {
        "chat" | "chat_completions" | "chat-completions" => {
            Some(echo_agent::llm::LlmApiProtocol::ChatCompletions)
        }
        "responses" => Some(echo_agent::llm::LlmApiProtocol::Responses),
        "anthropic" | "messages" => Some(echo_agent::llm::LlmApiProtocol::Anthropic),
        _ => None,
    }
}

fn push_tui_system_message(app: &mut TuiApp, content: impl Into<String>) {
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        content: content.into(),
    });
}

fn refresh_tui_models(app: &mut TuiApp, config: &echo_agent_app_core::api::config::EkoConfig) {
    app.configured_models = echo_agent_app_core::api::model_config::configured_model_views(config)
        .into_iter()
        .filter_map(|view| {
            echo_agent_app_core::api::model_config::resolve_runtime_model(config, Some(&view.id))
                .ok()
        })
        .collect();
    app.model = echo_agent_app_core::api::model_config::resolve_runtime_model(config, None)
        .map(|runtime| runtime.model)
        .unwrap_or_default();
}

async fn handle_tui_model_command(app: &mut TuiApp, args: &str) {
    let Some(app_state) = app.app_state.clone() else {
        push_tui_system_message(app, "Model configuration is unavailable in this runtime.");
        return;
    };
    let parts = args.split_whitespace().collect::<Vec<_>>();
    match parts.first().copied().unwrap_or("list") {
        "list" => {
            let config = app_state.config.app_config.read().await;
            let models = echo_agent_app_core::api::model_config::configured_model_views(&config)
                .into_iter()
                .map(|model| {
                    let active = if model.is_default { "*" } else { " " };
                    format!(
                        "{active} {}  {}  {:?}  {:?}",
                        model.id, model.model, model.api_protocol, model.input_modalities
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let content = if models.is_empty() {
                format!("Current model: {}\nNo configured models.", app.model)
            } else {
                format!("Current model: {}\nConfigured models:\n{models}", app.model)
            };
            push_tui_system_message(app, content);
        }
        "use" => {
            let Some(selector) = parts.get(1) else {
                push_tui_system_message(app, "Usage: /model use <model-id|model-name>");
                return;
            };
            match app_state.set_default_model_owned(*selector).await {
                Ok(receipt) => {
                    refresh_tui_models(app, &receipt.config);
                    push_tui_system_message(app, format!("Active model: {}", receipt.model_id));
                }
                Err(error) => push_tui_system_message(app, error.to_string()),
            }
        }
        "delete" => {
            let Some(model_id) = parts.get(1) else {
                push_tui_system_message(app, "Usage: /model delete <model-id>");
                return;
            };
            match app_state.delete_configured_model_owned(*model_id).await {
                Ok(receipt) => {
                    refresh_tui_models(app, &receipt.config);
                    push_tui_system_message(app, format!("Deleted model: {model_id}"));
                }
                Err(error) => push_tui_system_message(app, error.to_string()),
            }
        }
        "test" => {
            let Some(selector) = parts.get(1) else {
                push_tui_system_message(app, "Usage: /model test <model-id|model-name>");
                return;
            };
            let config = app_state.config.app_config.read().await.clone();
            let runtime = match echo_agent_app_core::api::model_config::resolve_runtime_model(
                &config,
                Some(selector),
            ) {
                Ok(runtime) => runtime,
                Err(error) => {
                    push_tui_system_message(app, error.to_string());
                    return;
                }
            };
            match echo_agent_app_core::api::infra::test_runtime_llm_connection(&runtime).await {
                Ok(result) => push_tui_system_message(
                    app,
                    format!(
                        "Connection succeeded: {} ({})",
                        result.model, result.response
                    ),
                ),
                Err(error) => {
                    push_tui_system_message(app, format!("Connection failed: {error}"));
                }
            }
        }
        "add" | "update" => {
            let (Some(provider), Some(model), Some(protocol)) = (
                parts.get(1),
                parts.get(2),
                parts.get(3).and_then(|value| parse_tui_llm_protocol(value)),
            ) else {
                push_tui_system_message(
                    app,
                    "Usage: /model add <provider-id> <model> <chat|responses|anthropic> [image] [audio] [video] [default]",
                );
                return;
            };
            let flags = parts.get(4..).unwrap_or(&[]);
            let mut input_modalities = echo_agent::llm::ModelInputModality::text_only();
            if flags.contains(&"image") {
                input_modalities.push(echo_agent::llm::ModelInputModality::Image);
            }
            if flags.contains(&"audio") {
                input_modalities.push(echo_agent::llm::ModelInputModality::Audio);
            }
            if flags.contains(&"video") {
                input_modalities.push(echo_agent::llm::ModelInputModality::Video);
            }
            let mutation = echo_agent_app_core::api::state::ConfiguredModelMutation {
                model: echo_agent_app_core::api::config::ConfiguredModel {
                    provider: (*provider).to_string(),
                    model: (*model).to_string(),
                    api_protocol: protocol,
                    input_modalities,
                    ..Default::default()
                },
                set_default: flags.contains(&"default"),
            };
            match app_state.upsert_configured_model_owned(mutation).await {
                Ok(receipt) => {
                    refresh_tui_models(app, &receipt.config);
                    push_tui_system_message(app, format!("Saved model: {}", receipt.model_id));
                }
                Err(error) => push_tui_system_message(app, error.to_string()),
            }
        }
        selector => match app_state.set_default_model_owned(selector).await {
            Ok(receipt) => {
                refresh_tui_models(app, &receipt.config);
                push_tui_system_message(app, format!("Active model: {}", receipt.model_id));
            }
            Err(error) => push_tui_system_message(app, error.to_string()),
        },
    }
}

async fn handle_tui_provider_command(app: &mut TuiApp, args: &str) {
    let Some(app_state) = app.app_state.clone() else {
        push_tui_system_message(
            app,
            "Provider configuration is unavailable in this runtime.",
        );
        return;
    };
    let parts = args.split_whitespace().collect::<Vec<_>>();
    match parts.first().copied().unwrap_or("list") {
        "list" => {
            let config = app_state.config.app_config.read().await;
            let providers =
                echo_agent_app_core::api::model_config::configured_provider_views(&config)
                    .into_iter()
                    .map(|provider| {
                        format!(
                            "{}  {}  {}  {:?}  {} models",
                            provider.id,
                            provider.name,
                            provider.base_url,
                            provider.default_api_protocol,
                            provider.model_count
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
            push_tui_system_message(
                app,
                if providers.is_empty() {
                    "No model providers configured.".to_string()
                } else {
                    format!("Model providers:\n{providers}")
                },
            );
        }
        "delete" => {
            let Some(provider_id) = parts.get(1) else {
                push_tui_system_message(app, "Usage: /provider delete <provider-id>");
                return;
            };
            match app_state.delete_model_provider_owned(*provider_id).await {
                Ok(receipt) => {
                    refresh_tui_models(app, &receipt.config);
                    push_tui_system_message(app, format!("Deleted provider: {provider_id}"));
                }
                Err(error) => push_tui_system_message(app, error.to_string()),
            }
        }
        "add" | "update" => {
            let (Some(id), Some(base_url), Some(protocol)) = (
                parts.get(1),
                parts.get(2),
                parts.get(3).and_then(|value| parse_tui_llm_protocol(value)),
            ) else {
                push_tui_system_message(
                    app,
                    "Usage: /provider add <id> <base-url> <chat|responses|anthropic> [api-key-env] [requires-key]",
                );
                return;
            };
            let api_key_env = parts
                .get(4)
                .filter(|value| !value.trim().is_empty() && **value != "-")
                .map(|value| (*value).to_string());
            let requires_api_key = parts.get(5).is_some_and(|value| *value == "requires-key");
            let mutation = echo_agent_app_core::api::state::ModelProviderMutation {
                id: (*id).to_string(),
                provider: echo_agent_app_core::api::config::ModelProviderConfig {
                    name: (*id).to_string(),
                    api_key_env,
                    base_url: Some((*base_url).to_string()),
                    default_api_protocol: Some(protocol),
                    requires_api_key,
                    ..Default::default()
                },
                preserve_auth_token: true,
            };
            match app_state.upsert_model_provider_owned(mutation).await {
                Ok(receipt) => {
                    refresh_tui_models(app, &receipt.config);
                    push_tui_system_message(app, format!("Saved provider: {}", receipt.model_id));
                }
                Err(error) => push_tui_system_message(app, error.to_string()),
            }
        }
        _ => push_tui_system_message(app, "Usage: /provider [list|add|update|delete]"),
    }
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
        Some(SlashCommand::Model) => handle_tui_model_command(app, args).await,
        Some(SlashCommand::Provider) => handle_tui_provider_command(app, args).await,
        Some(SlashCommand::Think) => {
            let available = if let Some(app_state) = app.app_state.as_ref() {
                let config = app_state.config.app_config.read().await;
                echo_agent_app_core::api::model_config::resolve_runtime_model(&config, None)
                    .map(|runtime| {
                        echo_agent_app_core::api::model_config::thinking_level_specs(
                            runtime.thinking_profile,
                        )
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
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
                    content: format!(
                        "Thinking configuration: {current}\nAvailable: {}",
                        if available.is_empty() {
                            "auto (model managed)".to_string()
                        } else {
                            format!("auto, {}", available.join(", "))
                        }
                    ),
                });
            } else {
                let requested = args.trim().to_ascii_lowercase();
                if requested != "auto" && !available.iter().any(|level| level == &requested) {
                    push_tui_system_message(
                        app,
                        format!(
                            "Thinking level '{requested}' is not available for the active model. Available: {}",
                            if available.is_empty() {
                                "auto".to_string()
                            } else {
                                format!("auto, {}", available.join(", "))
                            }
                        ),
                    );
                    return;
                }
                match echo_agent::llm::ThinkingConfig::parse_spec(&requested) {
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
            if args.trim().is_empty() {
                let pool_execution = match app.conversation_id.as_deref() {
                    Some(conversation_id) => {
                        tui_conversation_execution(app, conversation_id).await.ok()
                    }
                    None => None,
                };
                let active_agent = pool_execution
                    .as_ref()
                    .map(echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease::agent)
                    .unwrap_or_else(|| agent.clone());
                let prompt = active_agent
                    .read(|value| value.current_system_prompt())
                    .await;
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: prompt,
                });
            } else {
                if let Some(state) = app.app_state.as_ref() {
                    state
                        .apply_system_prompt_to_agents(args.trim().to_string())
                        .await;
                } else {
                    agent
                        .write_async(|value| {
                            let prompt = args.trim().to_string();
                            Box::pin(async move {
                                value.set_system_prompt(prompt).await;
                                let disabled_tools = value.disabled_tool_names();
                                echo_agent_app_core::api::subagent_prompt::refresh_primary_system_prompt(
                                    value,
                                    &disabled_tools,
                                );
                            })
                        })
                        .await;
                }
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "System prompt updated for this runtime.".to_string(),
                });
            }
        }
        Some(SlashCommand::Memory) => {
            let content = match current_tui_memory_control(app).await {
                Ok((_runtime, _generation, layer_manager)) => {
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
                Err(error) => error,
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Reflect) => {
            let reflection_input = if args.trim().is_empty() {
                "/reflect".to_string()
            } else {
                format!("/reflect {}", args.trim())
            };
            if let Err(error) =
                echo_agent_app_core::api::reflection::ReflectionCommand::parse(&reflection_input)
            {
                return push_system_message(app, error.to_string());
            }
            let runtime = match current_tui_control_runtime(app).await {
                Ok(runtime) => runtime,
                Err(error) => {
                    return push_system_message(app, format!("Reflection unavailable: {error}"));
                }
            };
            let conversation_id = app.conversation_id.clone();
            let content = match echo_agent_app_core::api::reflection::reflect_session(
                &runtime,
                agent,
                conversation_id.as_deref(),
            )
            .await
            {
                Ok(receipt) => render_tui_reflection_receipt(&receipt),
                Err(error) => format!("Reflection failed: {error}"),
            };
            push_system_message(app, content);
        }
        Some(SlashCommand::Remember) => {
            if args.trim().is_empty() {
                return push_system_message(app, "Usage: /remember <fact>".to_string());
            }
            let (_runtime, memory_generation, layer_manager) =
                match current_tui_memory_control(app).await {
                    Ok(control) => control,
                    Err(error) => {
                        return push_system_message(app, format!("Cannot save memory: {error}"));
                    }
                };
            let key = uuid::Uuid::new_v4().to_string();
            let meta = echo_agent::memory::MemoryMeta::new(
                echo_agent::memory::MemoryType::ProjectFact,
                echo_agent::memory::MemorySource::ExplicitSave,
                "explicit",
            );
            let content = match layer_manager.write_memory(&key, args.trim(), meta).await {
                Ok(_) => {
                    let projection = memory_generation.settle_hot_memory_projection().await;
                    match projection.error {
                        Some(error) => format!(
                            "Memory saved with key: {key}\nProjection remains pending: {error}"
                        ),
                        None => format!("Memory saved with key: {key}"),
                    }
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
            let (_runtime, memory_generation, layer_manager) =
                match current_tui_memory_control(app).await {
                    Ok(control) => control,
                    Err(error) => {
                        return push_system_message(app, format!("Cannot remove memory: {error}"));
                    }
                };
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
                Some(key) => match layer_manager.delete_memory(&key).await {
                    Ok(true) => {
                        let projection = memory_generation.settle_hot_memory_projection().await;
                        match projection.error {
                            Some(error) => format!(
                                "Removed memory: {key}\nProjection remains pending: {error}"
                            ),
                            None => format!("Removed memory: {key}"),
                        }
                    }
                    Ok(false) => "No matching memory found.".to_string(),
                    Err(error) => format!("Failed to remove memory: {error}"),
                },
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
                && let (Some(app_state), Some(id)) =
                    (app.app_state.as_ref(), app.conversation_id.as_deref())
            {
                let _ = app_state
                    .ensure_conversation_owned(echo_agent::memory::NewConversation {
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
            } else if let Err(error) = resume_conversation(app, args.trim()).await {
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
            let result = match app.app_state.as_ref() {
                _ if id.is_empty() => Err("Usage: /delete-session <conversation-id>".to_string()),
                Some(app_state) => app_state
                    .delete_conversation_owned(id)
                    .await
                    .map(|_| ())
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
        Some(SlashCommand::OpenArtifact) => {
            let requested = args.trim();
            let from_tool = app.messages.iter().rev().find_map(|message| {
                let MessageRole::ToolExecution(tool) = &message.role else {
                    return None;
                };
                if requested.is_empty() || tool.call_id == requested {
                    tool.artifact.as_ref().map(|artifact| artifact.path.clone())
                } else {
                    None
                }
            });
            let path = from_tool
                .or_else(|| (!requested.is_empty()).then(|| std::path::PathBuf::from(requested)));
            let result = match path {
                Some(path) => open_artifact_path(&path)
                    .map(|()| format!("Opened tool-output artifact: {}", path.display())),
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
            let result = match (app.app_state.as_ref(), app.conversation_id.as_ref()) {
                (Some(app_state), Some(conversation_id)) => app_state
                    .compress_conversation_owned(
                        echo_agent_app_core::api::manual_compression::ManualCompressionRequest {
                            workspace_id: app_state
                                .current_execution_scope()
                                .await
                                .workspace_id()
                                .to_string(),
                            conversation_id: conversation_id.clone(),
                            surface: ForegroundTurnSurface::Tui,
                            focus: None,
                            keep_messages: 12,
                        },
                    )
                    .await,
                _ => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: "压缩需要一个已持久化的活动会话。".to_string(),
                    });
                    return;
                }
            };
            match result {
                Ok(receipt) => {
                    app.context_snapshot.clear_usage();
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!(
                            "上下文已压缩: {} → {} 条消息, 节省 ≈{} tokens",
                            receipt.messages_before,
                            receipt.messages_after,
                            receipt.tokens_saved()
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
        Some(
            command @ (SlashCommand::Skills
            | SlashCommand::Mcp
            | SlashCommand::Hooks
            | SlashCommand::Plugins),
        ) => {
            let root = match command {
                SlashCommand::Skills => "skills",
                SlashCommand::Mcp => "mcp",
                SlashCommand::Hooks => "hooks",
                SlashCommand::Plugins => "plugins",
                _ => return,
            };
            let receipt = crate::cli::extension_surface::dispatch_extension_command(
                app.app_state.as_ref(),
                app.conversation_id.as_deref(),
                root,
                args,
            )
            .await;
            apply_tui_extension_receipt(app, &receipt);
            push_system_message(app, receipt.display_message());
        }
        Some(SlashCommand::Permission) => {
            if args.is_empty() {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Permission mode: {}", app.permission_mode),
                });
            } else {
                let framework_mode = match args
                    .trim()
                    .parse::<echo_agent::tools::permission::PermissionMode>()
                {
                    Ok(mode) => mode,
                    Err(_) => {
                        app.messages.push(ChatMessage {
                                role: MessageRole::System,
                                content: "Unknown permission mode; use default, plan, auto-edit, full-auto, auto, bubble, dont-ask, or strict".to_string(),
                            });
                        return;
                    }
                };
                let normalized = framework_mode.id();
                if let Some(state) = app.app_state.as_ref() {
                    *state.config.permission_mode.write().await = framework_mode;
                    state.apply_permission_mode_to_agents(framework_mode).await;
                } else {
                    agent
                        .write(|value| value.set_permission_mode(framework_mode))
                        .await;
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
            use echo_agent_app_core::api::auto_memory::{
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
                    let (runtime, evidence_generation, _layer_manager) =
                        match current_tui_memory_control(app).await {
                            Ok(control) => control,
                            Err(error) => {
                                return push_system_message(
                                    app,
                                    format!("Cannot inspect auto-memory: {error}"),
                                );
                            }
                        };
                    let scoped_agent = runtime.primary_agent();
                    let messages: Vec<(String, String)> = scoped_agent
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
                        let store = evidence_generation.evidence_store();
                        match queue_observations(&store, &observations, &messages) {
                            Ok(candidates) => {
                                let projection = if candidates.is_empty() {
                                    None
                                } else {
                                    Some(evidence_generation.settle_hot_memory_projection().await)
                                };
                                let mut message = format!(
                                    "Queued {} auto-memory candidate(s) in Review Inbox.",
                                    candidates.len()
                                );
                                if let Some(error) = projection.and_then(|receipt| receipt.error) {
                                    message.push_str(&format!(
                                        "\nProjection remains pending: {error}"
                                    ));
                                }
                                message
                            }
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
            let (runtime, review_lease, layer_manager) = match current_tui_memory_control(app).await
            {
                Ok(control) => control,
                Err(error) => {
                    app.messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Run review unavailable: {error}"),
                    });
                    return;
                }
            };
            let scoped_agent = runtime.primary_agent();
            let (run_store, llm_client) = scoped_agent
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
            let reviewer = reviewer.with_layer_manager(layer_manager);

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
            let settled = match review_lease.clone().track_background_review(handle).await {
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
                    let projection = if settlement.evidence_candidate.is_some() {
                        Some(review_lease.settle_hot_memory_projection().await)
                    } else {
                        None
                    };
                    let outcome = settlement.outcome;
                    let mut content = match outcome.candidate {
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
                    if let Some(error) = projection.and_then(|receipt| receipt.error) {
                        content.push_str(&format!("\nProjection remains pending: {error}"));
                    }
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
            use echo_agent_app_core::api::evolution::EvidenceReviewFilter;

            let mut parts = args.trim().splitn(3, ' ');
            let sub = parts
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or("list");
            let candidate_id = parts.next();
            let content = parts.next();
            let (_runtime, memory_generation, layer_manager) = match current_tui_memory_control(app)
                .await
            {
                Ok(control) => control,
                Err(error) => {
                    return push_system_message(app, format!("Review Inbox unavailable: {error}"));
                }
            };
            let store = memory_generation.evidence_store();
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
                                        echo_agent_app_core::api::evolution::EvidenceCandidateStatus::Applied
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
                        Ok(candidate) => {
                            let projection =
                                memory_generation.settle_hot_memory_projection().await;
                            match projection.error {
                                Some(error) => format!(
                                    "Updated {}.\nProjection remains pending: {error}",
                                    candidate.candidate_id
                                ),
                                None => format!("Updated {}.", candidate.candidate_id),
                            }
                        }
                        Err(error) => format!("Failed to edit candidate: {error}"),
                    },
                    _ => {
                        "Usage: /evidence-inbox edit <candidate-id> <new-content>".to_string()
                    }
                },
                "reject" => match candidate_id {
                    Some(id) => match store.reject(id) {
                        Ok(candidate) => {
                            let projection =
                                memory_generation.settle_hot_memory_projection().await;
                            match projection.error {
                                Some(error) => format!(
                                    "Rejected {}.\nProjection remains pending: {error}",
                                    candidate.candidate_id
                                ),
                                None => format!("Rejected {}.", candidate.candidate_id),
                            }
                        }
                        Err(error) => format!("Failed to reject candidate: {error}"),
                    },
                    None => "Usage: /evidence-inbox reject <candidate-id>".to_string(),
                },
                "accept" | "undo" => match candidate_id {
                    Some(id) => {
                        let action = if sub == "accept" {
                            store.accept(id, content, &layer_manager).await
                        } else {
                            store.undo(id, &layer_manager).await
                        };
                        match action {
                            Ok(candidate) => {
                                let projection =
                                    memory_generation.settle_hot_memory_projection().await;
                                match projection.error {
                                    Some(error) => format!(
                                        "{} is now {:?}.\nProjection remains pending: {error}",
                                        candidate.candidate_id, candidate.status
                                    ),
                                    None => format!(
                                        "{} is now {:?}.",
                                        candidate.candidate_id, candidate.status
                                    ),
                                }
                            }
                            Err(error) => format!("Review Inbox action failed: {error}"),
                        }
                    }
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
            let (runtime, memory_generation, _layer_manager) =
                match current_tui_memory_control(app).await {
                    Ok(control) => control,
                    Err(error) => {
                        return push_system_message(
                            app,
                            format!("Evolution dashboard unavailable: {error}"),
                        );
                    }
                };
            let scoped_agent = runtime.primary_agent();
            let run_store = scoped_agent.read(|value| value.run_store.clone()).await;
            let store = memory_generation.memory_store();
            let Some(integration) = runtime.review_integration() else {
                return push_system_message(
                    app,
                    "Review integration is not configured.".to_string(),
                );
            };
            let echo_agent_dir = integration.echo_agent_dir();
            let change_log = match echo_agent::evolution::JsonlChangeLog::new(
                echo_agent_dir.join("evolution").join("change-log.jsonl"),
            ) {
                Ok(change_log) => change_log,
                Err(error) => {
                    return push_system_message(
                        app,
                        format!("Failed to open evolution change log: {error}"),
                    );
                }
            };
            let dashboard = echo_agent_app_core::api::evolution::Dashboard::new(store, change_log)
                .with_run_store(run_store);
            let metrics = dashboard.generate_metrics().await;
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: echo_agent_app_core::api::evolution::Dashboard::format_metrics(&metrics),
            });
        }
        Some(SlashCommand::MemoryReview) => match current_tui_memory_control(app).await {
            Ok((runtime, _memory_generation, _layer_manager)) => {
                let Some(review_integration) = runtime.review_integration() else {
                    push_system_message(
                        app,
                        "Memory review integration is not configured.".to_string(),
                    );
                    return;
                };
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "📋 Running memory review...".to_string(),
                });

                match review_integration.run_review().await {
                    Ok(report) => {
                        let formatted =
                            echo_agent_app_core::api::evolution::format_review_report(&report);
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
            Err(error) => {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Memory review unavailable: {error}"),
                });
            }
        },
        Some(SlashCommand::SkillCandidates) => {
            // List candidates and drafts from Curator state
            let runtime = match current_tui_control_runtime(app).await {
                Ok(runtime) => runtime,
                Err(error) => {
                    return push_system_message(
                        app,
                        format!("Skill candidates unavailable: {error}"),
                    );
                }
            };
            let Some(integration) = runtime.review_integration() else {
                return push_system_message(
                    app,
                    "Review integration is not configured.".to_string(),
                );
            };
            let curator = integration.curator();
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
                refresh_workspace_generation(app, app_state.as_ref()).await;
            }
            push_system_message(app, result.output);
        }
        Some(SlashCommand::AgentList) => {
            let content =
                crate::cli::cmd_impls::agent_router::list_agent_endpoints(app.app_state.as_ref())
                    .await;
            push_system_message(app, content);
        }
        Some(SlashCommand::AgentSend) => {
            let parts = args.split_whitespace().collect::<Vec<_>>();
            let content = match (parts.first(), parts.get(1), parts.get(2..)) {
                (Some(workspace_id), Some(conversation_id), Some(message_parts))
                    if !message_parts.is_empty() =>
                {
                    let Some(state) = app.app_state.as_ref() else {
                        push_system_message(app, "Agent routing is not initialized.".to_string());
                        return;
                    };
                    let from = match state
                        .current_agent_address(app.conversation_id.as_deref())
                        .await
                    {
                        Ok(address) => address,
                        Err(error) => {
                            push_system_message(
                                app,
                                format!("Agent source resolution failed: {error}"),
                            );
                            return;
                        }
                    };
                    crate::cli::cmd_impls::agent_router::send_agent_text(
                        app.app_state.as_ref(),
                        from,
                        workspace_id,
                        conversation_id,
                        &message_parts.join(" "),
                    )
                    .await
                }
                _ => "Usage: /agent-send <workspace-id> <conversation-id> <message>".to_string(),
            };
            push_system_message(app, content);
        }
        Some(SlashCommand::AgentStatus) => {
            let parts = args.split_whitespace().collect::<Vec<_>>();
            let content = match (parts.first(), parts.get(1)) {
                (Some(workspace_id), Some(conversation_id)) => {
                    crate::cli::cmd_impls::agent_router::agent_delivery_status(
                        app.app_state.as_ref(),
                        workspace_id,
                        conversation_id,
                        parts.get(2).copied(),
                    )
                    .await
                }
                _ => {
                    "Usage: /agent-status <workspace-id> <conversation-id> [message-id]".to_string()
                }
            };
            push_system_message(app, content);
        }
        Some(SlashCommand::AgentGroup) => {
            let parts = args.split_whitespace().collect::<Vec<_>>();
            let content = crate::cli::cmd_impls::agent_router::execute_agent_group_command(
                app.app_state.as_ref(),
                &parts,
            )
            .await;
            push_system_message(app, content);
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
            let content = match app.app_state.as_ref() {
                Some(state) => match state.current_product_data().await {
                    Ok(product_data) => {
                        crate::cli::cmd_impls::analysis::execute_analysis_command(
                            &product_data,
                            &command_args,
                        )
                        .await
                    }
                    Err(error) => format!("Analysis workspace is unavailable: {error}"),
                },
                None => "Analysis workspace is unavailable.".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Papers) => {
            let command_args: Vec<&str> = args.split_whitespace().collect();
            let content = match app.app_state.as_ref() {
                Some(state) => match state.current_product_data().await {
                    Ok(product_data) => {
                        crate::cli::cmd_impls::research::execute_papers_command(
                            &product_data,
                            &command_args,
                        )
                        .await
                    }
                    Err(error) => format!("Research workspace is unavailable: {error}"),
                },
                None => "Research workspace is unavailable.".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(command @ (SlashCommand::Lsp | SlashCommand::Browser)) => {
            let root = match command {
                SlashCommand::Lsp => "lsp",
                SlashCommand::Browser => "browser",
                _ => return,
            };
            let receipt = crate::cli::extension_surface::dispatch_extension_command(
                app.app_state.as_ref(),
                app.conversation_id.as_deref(),
                root,
                args,
            )
            .await;
            push_system_message(app, receipt.display_message());
            app.rebuild_message_groups();
        }
        Some(SlashCommand::Terminal) => {
            let Some(state) = app.app_state.clone() else {
                push_system_message(
                    app,
                    "Developer tools are unavailable during application bootstrap.".to_string(),
                );
                app.rebuild_message_groups();
                return;
            };
            let parsed = match shell_words::split(args) {
                Ok(parsed) => parsed,
                Err(error) => {
                    push_system_message(app, format!("Invalid command arguments: {error}"));
                    app.rebuild_message_groups();
                    return;
                }
            };
            let parsed_refs = parsed.iter().map(String::as_str).collect::<Vec<_>>();
            let registry =
                echo_agent_app_core::api::developer_commands::DeveloperCommandRegistry::new(
                    state.terminal.clone(),
                    Some(state),
                );
            match registry.execute("terminal", &parsed_refs).await {
                Ok(output) => {
                    push_system_message(app, output.message);
                    if let Some(terminal_id) = output.attached_terminal {
                        app.terminal_output.clear();
                        app.active_terminal_id = Some(terminal_id);
                    }
                }
                Err(error) => push_system_message(app, format!("/terminal failed: {error}")),
            }
            app.rebuild_message_groups();
        }
        Some(SlashCommand::Trace) => {
            let run_store = agent.read(|value| value.run_store().cloned()).await;
            let content = match run_store {
                None => "Run store not configured.".to_string(),
                Some(store) => {
                    let diagnostic_id = if args.trim().is_empty() {
                        match echo_agent_app_core::api::observability::list_diagnostic_runs(
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
                            match echo_agent_app_core::api::observability::load_run_diagnostics(
                                store.as_ref(),
                                &diagnostic_id,
                                app.prompt_assembly.clone(),
                            )
                            .await
                            {
                                Ok(Some(diagnostics)) => {
                                    echo_agent_app_core::api::observability::format_run_diagnostics(
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
            let content = match app.app_state.as_ref() {
                Some(state) => {
                    app.tool_count = state
                        .get_tool_infos(agent)
                        .await
                        .map(|infos| infos.len())
                        .unwrap_or_default();
                    echo_agent_app_core::api::tool_control::execute_tool_control_command(
                        state, agent, args,
                    )
                    .await
                }
                None => {
                    let tools = agent.read(|value| value.tool_names()).await;
                    app.tool_count = tools.len();
                    if tools.is_empty() {
                        "No tools registered.".to_string()
                    } else {
                        format!("Registered tools ({}):\n{}", tools.len(), tools.join("\n"))
                    }
                }
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
        }
        Some(SlashCommand::Tasks) => {
            app.sidebar_visible = true;
            app.sidebar_tab = 2;
            refresh_task_runtime_view(app).await;
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
            if let Err(error) = submit_tui_conversation_input(
                app,
                agent,
                agent_tx.clone(),
                instruction.to_string(),
                attachments.clone(),
            )
            .await
            {
                app.pending_attachments.extend(attachments);
                push_system_message(app, format!("Steer submission failed: {error}"));
            }
        }
        Some(SlashCommand::TaskCancel)
        | Some(SlashCommand::TaskPause)
        | Some(SlashCommand::TaskResume) => {
            let Some(action) = slash_cmd else {
                return;
            };
            let (runtime, store) = match current_tui_task_runtime(app).await {
                Ok(control) => control,
                Err(error) => {
                    push_system_message(app, format!("Task runtime is unavailable: {error}"));
                    return;
                }
            };
            let requested_run_id = (!args.trim().is_empty()).then_some(args.trim());
            let run = match resolve_tui_task_run(app, &runtime, &store, requested_run_id).await {
                Ok(run) => run,
                Err(error) => {
                    push_system_message(app, format!("Task run action failed: {error}"));
                    return;
                }
            };
            let run_id = run.run_id.clone();
            let result = match action {
                SlashCommand::TaskCancel => {
                    let owned_run_id = run_id.clone();
                    tui_task_runtime_io(store.clone(), "cancel TUI TaskRun", move |store| {
                        store.request_cancel(&owned_run_id)
                    })
                    .await
                    .and_then(|cancelled| {
                        cancelled
                            .then_some("cancelled")
                            .ok_or_else(|| "run is not cancellable".to_string())
                    })
                }
                SlashCommand::TaskPause => {
                    let owned_run_id = run_id.clone();
                    tui_task_runtime_io(store.clone(), "pause TUI TaskRun", move |store| {
                        store.request_pause(&owned_run_id)
                    })
                    .await
                    .and_then(|paused| {
                        paused
                            .then_some("paused")
                            .ok_or_else(|| "run is not actively pausable".to_string())
                    })
                }
                SlashCommand::TaskResume => {
                    let owned_run_id = run_id.clone();
                    let resume_state = match tui_task_runtime_io(
                        store.clone(),
                        "load TUI TaskRun resume state",
                        move |store| store.get_run_state(&owned_run_id),
                    )
                    .await
                    {
                        Ok(Some(state)) => state,
                        Ok(None) => {
                            return push_system_message(app, "TaskRun not found".to_string());
                        }
                        Err(error) => {
                            return push_system_message(
                                app,
                                format!("TaskRun resume state unavailable: {error}"),
                            );
                        }
                    };
                    if resume_state
                        .continuation
                        .as_ref()
                        .is_some_and(|continuation| continuation.enabled)
                    {
                        match dispatch_turn(
                            app,
                            agent,
                            agent_tx.clone(),
                            TuiTurnRequest {
                                text: format!(
                                    "Continue the existing TaskRun {run_id} toward its unchanged Goal."
                                ),
                                attachments: Vec::new(),
                                run_resume: Some(crate::tui::TaskRunResumeWake {
                                    identity: echo_agent_app_core::api::tasks::task_runtime::TaskRunResumeIdentity::capture(
                                        &resume_state,
                                    ),
                                    is_continuation: true,
                                }),
                                input_attempt: None,
                            },
                        )
                        .await
                        {
                            TurnDispatchResult::Started => Ok("continuation submitted"),
                            TurnDispatchResult::Rejected { error, .. } => Err(error),
                        }
                    } else {
                        match dispatch_turn(
                            app,
                            agent,
                            agent_tx.clone(),
                            TuiTurnRequest {
                                text: format!(
                                    "Resume the existing TaskRun {run_id} toward its unchanged Goal."
                                ),
                                attachments: Vec::new(),
                                run_resume: Some(crate::tui::TaskRunResumeWake {
                                    identity: echo_agent_app_core::api::tasks::task_runtime::TaskRunResumeIdentity::capture(&resume_state),
                                    is_continuation: false,
                                }),
                                input_attempt: None,
                            },
                        )
                        .await
                        {
                            TurnDispatchResult::Started => Ok("planned resume submitted"),
                            TurnDispatchResult::Rejected { error, .. } => Err(error),
                        }
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
            refresh_task_runtime_view(app).await;
        }
        Some(SlashCommand::TaskBudget) => {
            let (runtime, store) = match current_tui_task_runtime(app).await {
                Ok(control) => control,
                Err(error) => {
                    push_system_message(app, format!("Task runtime is unavailable: {error}"));
                    return;
                }
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
            let requested_run_id = values.next();
            let result = match resolve_tui_task_run(app, &runtime, &store, requested_run_id).await {
                Ok(run) => match parse_tui_budget(token_value, "token").and_then(|tokens| {
                    parse_tui_budget(time_value, "time").map(|time| (tokens, time))
                }) {
                    Ok((tokens, time)) => {
                        let run_id = run.run_id;
                        let owned_run_id = run_id.clone();
                        tui_task_runtime_io(
                            store.clone(),
                            "update TUI TaskRun budgets",
                            move |store| {
                                store.update_run_continuation_budgets(&owned_run_id, tokens, time)
                            },
                        )
                        .await
                        .map(|_| run_id)
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(run_id) => format!("Task run {run_id} budgets updated."),
                    Err(error) => format!("Task budget update failed: {error}"),
                },
            });
            refresh_task_runtime_view(app).await;
        }
        Some(SlashCommand::TaskGoal) => {
            let (runtime, store) = match current_tui_task_runtime(app).await {
                Ok(control) => control,
                Err(error) => {
                    push_system_message(app, format!("Task runtime is unavailable: {error}"));
                    return;
                }
            };
            let values = args.split_whitespace().collect::<Vec<_>>();
            let result = match crate::task_run_control::parse_run_goal_update_args(&values) {
                Ok(parsed) => match resolve_tui_task_run(
                    app,
                    &runtime,
                    &store,
                    parsed.requested_run_id.as_deref(),
                )
                .await
                {
                    Ok(run) => {
                        let run_id = run.run_id;
                        tui_task_runtime_io(
                            store.clone(),
                            "update TUI TaskRun Goal",
                            move |store| {
                                store.update_run_goal(
                                &run_id,
                                parsed.expected_goal_revision,
                                &parsed.new_goal,
                                &parsed.reason,
                                echo_agent_app_core::api::tasks::task_runtime::RunGoalActorSource::Tui,
                            )
                            },
                        )
                        .await
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
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
            refresh_task_runtime_view(app).await;
        }
        Some(SlashCommand::TaskRequirements) => {
            let (runtime, store) = match current_tui_task_runtime(app).await {
                Ok(control) => control,
                Err(error) => {
                    push_system_message(app, format!("Task runtime is unavailable: {error}"));
                    return;
                }
            };
            let requested_run_id = args.split_whitespace().next();
            let result = match resolve_tui_task_run(app, &runtime, &store, requested_run_id).await {
                Ok(run) => {
                    let run_id = run.run_id;
                    tui_task_runtime_io(store.clone(), "load TUI completion gate", move |store| {
                        store.completion_gate_report(&run_id)
                    })
                    .await
                }
                Err(error) => Err(error),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(report) => format_completion_gate(&report),
                    Err(error) => format!("Completion gate read failed: {error}"),
                },
            });
        }
        Some(SlashCommand::TaskRequirementSkip) => {
            let (runtime, store) = match current_tui_task_runtime(app).await {
                Ok(control) => control,
                Err(error) => {
                    push_system_message(app, format!("Task runtime is unavailable: {error}"));
                    return;
                }
            };
            let values = args.split_whitespace().collect::<Vec<_>>();
            let result = match crate::task_run_control::parse_requirement_skip_args(&values) {
                Ok(parsed) => match resolve_tui_task_run(
                    app,
                    &runtime,
                    &store,
                    parsed.requested_run_id.as_deref(),
                )
                .await
                {
                    Ok(run) => {
                        let run_id = run.run_id;
                        tui_task_runtime_io(
                            store.clone(),
                            "skip TUI Goal requirement",
                            move |store| {
                                store.skip_goal_requirement(
                                &run_id,
                                parsed.expected_goal_revision,
                                &parsed.requirement_id,
                                &parsed.reason,
                                echo_agent_app_core::api::tasks::task_runtime::RunGoalActorSource::Tui,
                            )
                            },
                        )
                        .await
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(report) => format_completion_gate(&report),
                    Err(error) => format!("Requirement Skip failed: {error}"),
                },
            });
            refresh_task_runtime_view(app).await;
        }
        Some(
            action @ (SlashCommand::SubagentMessage
            | SlashCommand::SubagentFollowup
            | SlashCommand::SubagentInterrupt),
        ) => {
            let (runtime, store) = match current_tui_task_runtime(app).await {
                Ok(control) => control,
                Err(error) => {
                    push_system_message(app, format!("Task runtime is unavailable: {error}"));
                    return;
                }
            };
            let (usage, instruction_required) = match action {
                SlashCommand::SubagentMessage => {
                    (crate::task_run_control::SUBAGENT_MESSAGE_USAGE, true)
                }
                SlashCommand::SubagentFollowup => {
                    (crate::task_run_control::SUBAGENT_FOLLOWUP_USAGE, true)
                }
                SlashCommand::SubagentInterrupt => {
                    (crate::task_run_control::SUBAGENT_INTERRUPT_USAGE, false)
                }
                _ => return,
            };
            let values = args.split_whitespace().collect::<Vec<_>>();
            let parsed = crate::task_run_control::parse_subagent_control_args(
                &values,
                usage,
                instruction_required,
            );
            if let Ok(parsed) = parsed.as_ref()
                && let Err(error) =
                    resolve_tui_task_run(app, &runtime, &store, Some(&parsed.identity.run_id)).await
            {
                push_system_message(app, format!("Subagent control failed: {error}"));
                return;
            }
            let result = match parsed {
                Ok(parsed) => {
                    let service =
                        echo_agent_app_core::api::tasks::task_runtime::SubagentControlService::new(
                            store,
                        );
                    match action {
                        SlashCommand::SubagentMessage => {
                            let Some(instruction) = parsed.instruction.as_deref() else {
                                app.messages.push(ChatMessage {
                                    role: MessageRole::System,
                                    content: format!("Usage: {usage}"),
                                });
                                return;
                            };
                            service
                                .send_message(
                                    parsed.identity,
                                    instruction,
                                    echo_agent_app_core::api::tasks::task_runtime::SubagentControlActorSource::Tui,
                                )
                                .await
                        }
                        SlashCommand::SubagentFollowup => {
                            let Some(instruction) = parsed.instruction.as_deref() else {
                                app.messages.push(ChatMessage {
                                    role: MessageRole::System,
                                    content: format!("Usage: {usage}"),
                                });
                                return;
                            };
                            service
                                .queue_guidance_async(
                                    parsed.identity,
                                    instruction.to_string(),
                                    echo_agent_app_core::api::tasks::task_runtime::SubagentControlActorSource::Tui,
                                )
                                .await
                        }
                        SlashCommand::SubagentInterrupt => {
                            service
                                .interrupt_subagent(
                                    parsed.identity,
                                    echo_agent_app_core::api::tasks::task_runtime::SubagentControlActorSource::Tui,
                                )
                                .await
                        }
                        _ => return,
                    }
                }
                Err(error) => Err(
                    echo_agent_app_core::api::tasks::task_runtime::StoreError::InvalidPlan(error),
                ),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(receipt) => format!(
                        "Subagent command {} is {}{}.",
                        receipt.identity.command_id,
                        receipt.status.as_str(),
                        receipt
                            .detail
                            .as_deref()
                            .map(|detail| format!(": {detail}"))
                            .unwrap_or_default()
                    ),
                    Err(error) => format!("Subagent control failed: {error}"),
                },
            });
            refresh_task_runtime_view(app).await;
        }
        Some(SlashCommand::TaskRecovery) => {
            let (runtime, store) = match current_tui_task_runtime(app).await {
                Ok(control) => control,
                Err(error) => {
                    push_system_message(app, format!("Task runtime is unavailable: {error}"));
                    return;
                }
            };
            let requested_run_id = (!args.trim().is_empty()).then_some(args.trim());
            let run = match resolve_tui_task_run(app, &runtime, &store, requested_run_id).await {
                Ok(run) => run,
                Err(error) => {
                    push_system_message(app, format!("Recovery read failed: {error}"));
                    return;
                }
            };
            let run_id = run.run_id;
            let owned_run_id = run_id.clone();
            let content = match tui_task_runtime_io(
                store,
                "load TUI recovery blockers",
                move |store| store.list_recovery_blockers(&owned_run_id),
            )
            .await
            {
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
            let (runtime, store) = match current_tui_task_runtime(app).await {
                Ok(control) => control,
                Err(error) => {
                    push_system_message(app, format!("Task runtime is unavailable: {error}"));
                    return;
                }
            };
            let mut parts = args.split_whitespace();
            let Some(task_id) = parts.next() else {
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Usage: {} <task-id> [run-id]", action.slash_name()),
                });
                return;
            };
            let requested_run_id = parts.next();
            let run = match resolve_tui_task_run(app, &runtime, &store, requested_run_id).await {
                Ok(run) => run,
                Err(error) => {
                    push_system_message(app, format!("Task recovery control failed: {error}"));
                    return;
                }
            };
            let run_id = run.run_id.clone();
            let result = if action == SlashCommand::TaskRetry {
                let pool_execution = runtime
                    .agent_for(&run.conversation_id)
                    .await
                    .map_err(|error| error.to_string());
                let pool_execution = match pool_execution {
                    Ok(execution) => execution,
                    Err(error) => {
                        app.messages.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!("Failed to resolve TaskRun Agent: {error}"),
                        });
                        return;
                    }
                };
                retry_tui_task(
                    store.clone(),
                    pool_execution.agent(),
                    Some(pool_execution),
                    run_id.clone(),
                    task_id.to_string(),
                    runtime.review_integration(),
                    Some(runtime.workspace_io_invocation()),
                )
                .await
                .map_err(|error| error.to_string())
            } else {
                let owned_run_id = run_id.clone();
                let owned_task_id = task_id.to_string();
                tui_task_runtime_io(store.clone(), "resolve TUI recovery task", move |store| {
                    store.resolve_recovery_task(
                        &owned_run_id,
                        &owned_task_id,
                        echo_agent_app_core::api::tasks::task_runtime::RecoveryDecision::Skip,
                    )
                })
                .await
                .map(|()| format!("Recovery decision recorded for {run_id}/{task_id}: skip."))
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content: match result {
                    Ok(msg) => msg,
                    Err(error) => format!("Failed to retry/skip task: {error}"),
                },
            });
            refresh_task_runtime_view(app).await;
        }
        Some(SlashCommand::Preview) => {
            match resolve_tui_workspace_file(app.workspace_execution_scope.root(), args) {
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
        Some(SlashCommand::Edit) => {
            match resolve_tui_workspace_file(app.workspace_execution_scope.root(), args) {
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
            }
        }
        Some(SlashCommand::Worktrees) => {
            let content = handle_tui_worktrees(app, args).await;
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
            app.rebuild_message_groups();
        }
        Some(SlashCommand::Workflow) => {
            let content = match app.app_state.as_ref() {
                Some(state) => state
                    .history
                    .workflows
                    .execute_command(args)
                    .await
                    .unwrap_or_else(|error| format!("Workflow command failed: {error}")),
                None => "Workflow service is unavailable.".to_string(),
            };
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                content,
            });
            app.rebuild_message_groups();
        }
        Some(SlashCommand::Extract) => {
            let state = app.app_state.clone();
            let conversation_id = app.conversation_id.clone();
            let workspace_id = app.workspace_execution_scope.workspace_id().to_string();
            let content = match (state, conversation_id) {
                (Some(state), Some(conversation_id)) => state
                    .execute_structured_extraction_command_for_scope(
                        &workspace_id,
                        &conversation_id,
                        echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Tui,
                        args,
                    )
                    .await
                    .unwrap_or_else(|error| {
                        format!("Structured extraction command failed: {error}")
                    }),
                _ => "Structured extraction service is unavailable.".to_string(),
            };
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
            if let Err(error) =
                submit_tui_conversation_input(app, agent, agent_tx, prompt, Vec::new()).await
            {
                push_system_message(
                    app,
                    format!("Conversation input submission failed: {error}"),
                );
            }
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

async fn retry_tui_task(
    store: Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
    agent: AgentHandle,
    pool_execution: Option<echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease>,
    run_id: String,
    task_id: String,
    review_integration: Option<Arc<echo_agent_app_core::api::evolution::ReviewIntegration>>,
    workspace_io: Option<echo_agent_app_core::api::state::WorkspaceIoInvocation>,
) -> Result<String, echo_agent_app_core::api::tasks::task_runtime::StoreError> {
    let preparation = start_tui_task_retry_driver(
        store,
        agent,
        pool_execution,
        run_id.clone(),
        task_id.clone(),
        review_integration,
        workspace_io,
    )
    .await?;
    match preparation {
        echo_agent_app_core::api::tasks::task_runtime::TaskRetryPreparation::Acceptance {
            next_attempt,
        } => Ok(format!(
            "Task {task_id} retried as attempt {next_attempt} on run {run_id}; executor started."
        )),
        echo_agent_app_core::api::tasks::task_runtime::TaskRetryPreparation::Recovery => Ok(
            format!("Recovery decision recorded for {run_id}/{task_id}: retry."),
        ),
    }
}

async fn start_tui_task_retry_driver(
    store: Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
    agent: AgentHandle,
    pool_execution: Option<echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease>,
    run_id: String,
    task_id: String,
    review_integration: Option<Arc<echo_agent_app_core::api::evolution::ReviewIntegration>>,
    workspace_io: Option<echo_agent_app_core::api::state::WorkspaceIoInvocation>,
) -> Result<
    echo_agent_app_core::api::tasks::task_runtime::TaskRetryPreparation,
    echo_agent_app_core::api::tasks::task_runtime::StoreError,
> {
    let cancel = echo_agent::agent::CancellationToken::new();
    let preparation_store = store.clone();
    let preparation_run_id = run_id.clone();
    let (preparation, _) = store
        .spawn_supervised_task_retry_async(
            run_id,
            task_id,
            cancel.clone(),
            move || {
                review_integration
                    .as_ref()
                    .map(|integration| integration.lease_generation())
                    .transpose()
                    .map_err(|error| {
                        echo_agent_app_core::api::tasks::task_runtime::StoreError::InvalidPlan(format!(
                            "memory generation unavailable: {error}"
                        ))
                    })
            },
            move |memory_generation, mut receipt_owner| async move {
                let _pool_execution = pool_execution;
                if let Some(generation) = memory_generation.as_ref() {
                    receipt_owner.retain(generation.clone());
                }
                let reviewer_llm = agent.read(|value| value.llm_client().cloned()).await;
                let run_store = agent.read(|value| value.run_store().cloned()).await;
                let result = echo_agent_app_core::api::tasks::task_runtime::execute_run(
                    preparation_store,
                    Some(agent),
                    reviewer_llm,
                    memory_generation,
                    run_store,
                    None,
                    &preparation_run_id,
                    cancel,
                    echo_agent_app_core::api::tasks::task_runtime::MemoryPolicy::BestEffortSettled,
                    workspace_io,
                )
                .await;
                if let Err(error) = result {
                    tracing::error!(run_id = %preparation_run_id, %error, "TUI task retry driver failed");
                    return Err(error.to_string());
                }
                Ok(())
            },
        )
        .await?;
    Ok(preparation)
}

fn resolve_tui_workspace_file(
    root: &std::path::Path,
    value: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("a file path is required"));
    }
    let root = root.canonicalize()?;
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
    }
    let pool_execution = match app.conversation_id.as_deref() {
        Some(conversation_id) => tui_conversation_execution(app, conversation_id).await.ok(),
        None => None,
    };
    let active_agent = pool_execution
        .as_ref()
        .map(echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease::agent)
        .unwrap_or_else(|| agent.clone());
    app.messages.clear();
    app.tokens = (0, 0, 0);
    app.streaming_text.clear();
    app.pending_stream.clear();
    if let Err(error) = app.discard_unsubmitted_attachments() {
        tracing::warn!(%error, "failed to clean staged attachments during TUI reset");
    }
    app.active_turn_id = None;
    app.active_turn_workspace_id = None;
    app.active_turn_conversation_id = None;
    app.active_turn_execution_root = None;
    app.active_turn_agent = None;
    app.task_runtime_view = None;
    app.subagent_runs.clear();
    app.is_processing = false;
    app.chat_scroll = 0;
    app.clear_selection();
    app.context_snapshot.clear_usage();
    app.usage_accumulator.reset();
    active_agent
        .read_async(|value| {
            Box::pin(async move {
                use echo_agent::agent::Agent;
                value.reset().await;
            })
        })
        .await;
}

async fn resume_conversation(app: &mut TuiApp, conversation_id: &str) -> anyhow::Result<()> {
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
    let pool_execution = tui_conversation_execution(app, conversation_id).await?;
    let active_agent = pool_execution.agent();
    active_agent
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
    let source_execution = match app.conversation_id.as_deref() {
        Some(conversation_id) => Some(tui_conversation_execution(app, conversation_id).await?),
        None => None,
    };
    let source_agent = source_execution
        .as_ref()
        .map(echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease::agent)
        .unwrap_or_else(|| agent.clone());
    let runtime_messages = source_agent
        .read_async(|value| Box::pin(async move { value.get_messages().await }))
        .await;
    drop(source_execution);
    let projected = echo_agent::memory::project_messages(&id, &runtime_messages)?;
    let default_title = app
        .conversation_id
        .as_deref()
        .map(|source| format!("Fork of {}", source.chars().take(8).collect::<String>()))
        .unwrap_or_else(|| "Forked conversation".to_string());
    let app_state = app
        .app_state
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("TUI application state is unavailable"))?;
    app_state
        .create_conversation_owned(echo_agent::memory::NewConversation {
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
    let target_execution = tui_conversation_execution(app, &id).await?;
    target_execution
        .agent()
        .read_async(|value| {
            Box::pin(async move {
                value.load_messages(runtime_messages).await;
            })
        })
        .await;
    app.conversation_id = Some(id.clone());
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        content: format!("Forked into conversation: {id}"),
    });
    Ok(())
}

async fn tui_conversation_execution(
    app: &TuiApp,
    conversation_id: &str,
) -> anyhow::Result<echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease> {
    let app_state = app
        .app_state
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("TUI application state is unavailable"))?;
    let runtime = app_state
        .current_control_runtime()
        .await
        .map_err(anyhow::Error::msg)?;
    runtime
        .agent_for(conversation_id)
        .await
        .map_err(anyhow::Error::msg)
}

/// Join immutable plan specifications with the canonical Todo projection.
///
/// `list_todos` owns dependency-derived badges (including failed-ancestor
/// blocking) and runtime metadata; this helper only aligns those rows with the
/// plan's stable display order and EKO role/title fields.
fn project_tui_task_views(
    plan: &echo_agent_app_core::api::tasks::task_runtime::PlanRevision,
    todos: &[echo_agent_app_core::api::tasks::task_runtime::TodoItem],
) -> Vec<TaskRuntimeTaskView> {
    plan.tasks
        .iter()
        .filter_map(|spec| {
            todos
                .iter()
                .find(|todo| todo.task_id == spec.id)
                .map(|todo| TaskRuntimeTaskView {
                    title: spec.title.clone(),
                    status: todo.status.as_str().to_string(),
                    agent_role: spec.agent_role.clone(),
                    owner_agent: todo.owner_agent.clone(),
                    started_at: todo.started_at,
                    completed_at: todo.completed_at,
                    summary: todo.summary.clone(),
                })
        })
        .collect()
}

async fn refresh_task_runtime_view(app: &mut TuiApp) {
    let (runtime, store) = match current_tui_task_runtime(app).await {
        Ok(control) => control,
        Err(error) => {
            tracing::debug!(%error, "TUI TaskRuntime projection is unavailable");
            app.task_runtime_view = None;
            return;
        }
    };
    let conversation_id = runtime
        .primary_agent()
        .read(|agent| agent.conversation_id().map(str::to_string))
        .await
        .filter(|conversation_id| !conversation_id.trim().is_empty());
    let Some(conversation_id) = conversation_id else {
        app.task_runtime_view = None;
        return;
    };
    let lookup_conversation_id = conversation_id.clone();
    let projection =
        tui_task_runtime_io(store, "refresh TUI TaskRuntime projection", move |store| {
            let Some(run) = store.latest_run_for_conversation(&lookup_conversation_id)? else {
                return Ok(None);
            };
            let plan = store.get_plan_revision(&run.run_id)?;
            let todos = store.list_todos(&run.run_id)?;
            let continuation = store
                .get_run_state(&run.run_id)?
                .and_then(|state| state.continuation);
            let active_cell_count = store
                .list_background_cells(&run.run_id)?
                .iter()
                .filter(|cell| cell.is_active())
                .count();
            let completion = store.completion_gate_report(&run.run_id)?;
            Ok(Some((
                run,
                plan,
                todos,
                continuation,
                active_cell_count,
                completion,
            )))
        })
        .await;
    let Some((run, plan, todos, continuation, active_cell_count, completion)) = (match projection {
        Ok(projection) => projection,
        Err(error) => {
            tracing::warn!(%error, "TUI failed to refresh TaskRuntime projection");
            return;
        }
    }) else {
        app.task_runtime_view = None;
        return;
    };
    let tasks = plan
        .map(|plan| project_tui_task_views(&plan, &todos))
        .unwrap_or_default();
    let completion_ready = completion.ready;
    let requirements = completion
        .requirements
        .into_iter()
        .map(|item| TaskRuntimeRequirementView {
            requirement_id: item.requirement.requirement_id,
            title: item.requirement.title,
            status: item.status.as_str().to_string(),
        })
        .collect();
    app.task_runtime_view = Some(TaskRuntimeView {
        workspace_id: runtime.execution_scope().workspace_id().to_string(),
        conversation_id,
        run_id: run.run_id,
        run_created_at: run.created_at,
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
        completion_ready,
        requirements,
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
    content.push_str(&format!(
        "\nCompletion gate: {}",
        if view.completion_ready {
            "ready"
        } else {
            "blocked"
        }
    ));
    for requirement in &view.requirements {
        content.push_str(&format!(
            "\n  [{}] {}: {}",
            requirement.status, requirement.requirement_id, requirement.title
        ));
    }
    content
}

fn format_completion_gate(
    report: &echo_agent_app_core::api::tasks::task_runtime::CompletionGateReport,
) -> String {
    let mut content = format!(
        "Completion gate: Goal r{}, Plan r{} ({})",
        report.goal_revision,
        report.plan_revision,
        if report.ready { "ready" } else { "blocked" }
    );
    for requirement in &report.requirements {
        content.push_str(&format!(
            "\n[{}] {}: {}",
            requirement.status.as_str(),
            requirement.requirement.requirement_id,
            requirement.requirement.title
        ));
    }
    for blocker in &report.blockers {
        content.push_str(&format!("\nBLOCK {:?}: {}", blocker.code, blocker.detail));
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

fn apply_tui_plugin_theme(
    app: &mut TuiApp,
    theme: Option<&echo_agent_app_core::api::extension_commands::PluginThemeProjection>,
) {
    app.theme = theme.map_or_else(
        || app.default_theme.clone(),
        |theme| {
            crate::tui::Theme::from_plugin_theme(
                &echo_agent_app_core::api::plugin_runtime::PluginThemeDefinition {
                    name: theme.name.clone(),
                    display_name: theme.display_name.clone(),
                    dark: theme.dark,
                    colors: theme.colors.clone(),
                    plugin: theme.plugin.clone(),
                },
            )
        },
    );
    app.rebuild_message_groups();
}

fn apply_tui_extension_receipt(
    app: &mut TuiApp,
    receipt: &echo_agent_app_core::api::extension_commands::ExtensionCommandReceipt,
) {
    use echo_agent_app_core::api::extension_commands::{
        ExtensionCommandReceipt, ExtensionCommandStatus, PluginCommandReceipt,
    };

    if receipt.status() == ExtensionCommandStatus::Failed {
        return;
    }
    let ExtensionCommandReceipt::Plugins {
        receipt: Some(plugin_receipt),
        ..
    } = receipt
    else {
        return;
    };
    match plugin_receipt {
        PluginCommandReceipt::Mutation { projection } => {
            if let Some(active) = projection.active_theme.as_deref() {
                if let Some(theme) = projection
                    .themes
                    .items
                    .iter()
                    .find(|theme| theme.name == active)
                {
                    apply_tui_plugin_theme(app, Some(theme));
                }
            } else {
                apply_tui_plugin_theme(app, None);
            }
        }
        PluginCommandReceipt::Theme { active, theme } if active.is_none() || theme.is_some() => {
            apply_tui_plugin_theme(app, theme.as_ref());
        }
        _ => {}
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
            outcome,
            ..
        } => {
            if let Some(run) = find_subagent_run_mut(app, execution_id.as_deref(), agent) {
                run.status = outcome.status.as_str().to_string();
                run.duration_ms = Some(*duration_ms);
                run.tokens_used = *tokens_used;
                apply_subagent_outcome(run, outcome);
            }
        }
        SubagentEvent::DispatchFailed {
            agent,
            execution_id,
            status,
            outcome,
            ..
        } => {
            if let Some(run) = find_subagent_run_mut(app, execution_id.as_deref(), agent) {
                run.status = status.as_str().to_string();
                apply_subagent_outcome(run, outcome);
            }
        }
        SubagentEvent::DispatchCancelled {
            agent,
            execution_id,
            outcome,
            ..
        } => {
            if let Some(run) = find_subagent_run_mut(app, execution_id.as_deref(), agent) {
                run.status = "cancelled".to_string();
                apply_subagent_outcome(run, outcome);
            }
        }
        _ => {}
    }
    if app.subagent_runs.len() > 50 {
        let remove = app.subagent_runs.len().saturating_sub(50);
        app.subagent_runs.drain(..remove);
    }
}

fn apply_subagent_outcome(
    run: &mut SubagentRuntimeView,
    outcome: &echo_agent::subagent::SubagentOutcome,
) {
    run.summary = outcome.summary.clone();
    run.artifacts = outcome
        .artifacts
        .iter()
        .map(|artifact| artifact.path.clone())
        .collect();
    run.verification = outcome
        .verification()
        .iter()
        .map(|item| format!("{}: {:?}", item.check, item.status))
        .collect();
    run.remaining_work = outcome.remaining_work.clone();
    let touched_files = outcome.touched_files();
    run.files_read = touched_files.read.clone();
    run.files_written = touched_files.written.clone();
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

#[cfg(test)]
mod tests {
    use super::{
        AgentEvent, ConversationInputAddress, ConversationInputFact, ConversationInputPhase,
        ConversationInputProjection, ForegroundTurnSnapshot, ForegroundTurnSurface,
        RegisteredTuiSteerError, apply_turn_settlement, complete_file_reference,
        delete_previous_word, exact_active_turn_for_address, exact_conversation_input_attempt,
        execute_registered_tui_steer, format_conversation_input_fact, format_task_runtime_view,
        format_unattended_worktrees, handle_approval_key, handle_esc, move_cursor_vertical,
        project_tui_task_views, render_cancelled_event, render_conversation_input_receipt,
        render_error_event, request_from_prepared, resolve_tui_workspace_file, retry_tui_task,
        reverse_history_search, run_turn_binding_for_request, settle_planned_resume_foreground,
        slash_command_allowed_while_busy, update_subagent_runs, validate_tui_task_run_scope,
    };
    use crate::tui::{
        ChatMessage, MessageRole, TaskRunResumeWake, TaskRuntimeRequirementView,
        TaskRuntimeTaskView, TaskRuntimeView, Theme, ToolExecutionMessage, ToolExecutionStatus,
        TuiApp, TuiTurnRequest,
    };
    use echo_agent_app_core::api::chat_driver::TurnOutcome;
    use echo_agent_app_core::api::tasks::task_runtime::{
        AttendedMode, DomainProfile, ExecutionMode, PlanRevision, PlanTask, TaskPlan,
        TaskRunStatus, TaskRuntimeStore, TodoItem, TodoStatus, commit_task_plan,
    };
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::mpsc;

    fn app() -> TuiApp {
        let theme = Theme::dark();
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
        commit_task_plan(
            store.clone(),
            TaskPlan {
                plan_id: "tui-retry-plan".to_string(),
                run_id: "tui-retry-closed".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: echo_agent_app_core::api::tasks::task_runtime::task_goal_sha256(
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
                echo_agent::tasks::TaskStatus::Failed(String::new()),
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
            None,
            "tui-retry-closed".to_string(),
            "retry-task".to_string(),
            None,
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
    fn only_matching_settlement_releases_the_tui_slot_once() {
        let mut app = app();
        app.is_processing = true;
        app.active_turn_id = Some("turn-1".to_string());

        assert!(!apply_turn_settlement(
            &mut app,
            "stale-turn",
            &TurnOutcome::Completed
        ));
        assert!(app.is_processing);
        assert_eq!(app.active_turn_id.as_deref(), Some("turn-1"));

        assert!(apply_turn_settlement(
            &mut app,
            "turn-1",
            &TurnOutcome::Cancelled
        ));
        assert!(!app.is_processing);
        assert!(app.active_turn_id.is_none());
        assert_eq!(app.status_msg, "Cancelled");

        assert!(!apply_turn_settlement(
            &mut app,
            "turn-1",
            &TurnOutcome::Cancelled
        ));
    }

    #[test]
    fn task_run_resume_wake_builds_exact_run_turn_binding() -> Result<(), String> {
        let turn = TuiTurnRequest {
            text: "continue".to_string(),
            attachments: Vec::new(),
            run_resume: Some(TaskRunResumeWake {
                identity: echo_agent_app_core::api::tasks::task_runtime::TaskRunResumeIdentity {
                    run_id: "exact-run".to_string(),
                    workspace_id: "workspace-a".to_string(),
                    conversation_id: "conversation-a".to_string(),
                    root_message_id: "exact-root-message".to_string(),
                    created_at: chrono::Utc::now(),
                    goal_revision: 1,
                    journal_sequence: 7,
                    continuation_enabled: true,
                },
                is_continuation: true,
            }),
            input_attempt: None,
        };

        let binding = run_turn_binding_for_request(&turn, "new-turn")
            .ok_or_else(|| "resume binding missing".to_string())?;
        assert_eq!(
            turn.run_resume
                .as_ref()
                .map(|resume| resume.identity.workspace_id.as_str()),
            Some("workspace-a")
        );
        assert_eq!(binding.run_id.as_deref(), Some("exact-run"));
        assert_eq!(binding.turn_id, "new-turn");
        assert_eq!(binding.root_message_id, "exact-root-message");
        assert_eq!(
            binding.origin,
            echo_agent_app_core::api::tasks::task_runtime::RunTurnOrigin::Resume
        );
        assert_eq!(
            binding.transcript_visibility,
            echo_agent_app_core::api::tasks::task_runtime::TurnVisibility::Visible
        );
        Ok(())
    }

    #[test]
    fn prepared_request_preserves_scoped_attachment_identity() {
        let staged = std::path::PathBuf::from("/workspace/.eko/uploads/staged.txt");
        let scoped = std::path::PathBuf::from(
            "/workspace/.eko/artifacts/user-input/conversation/turn/scoped.txt",
        );
        let request = TuiTurnRequest {
            text: "original".to_string(),
            attachments: vec![echo_agent_app_core::api::attachments::AttachmentRef {
                path: staged,
                name: "input.txt".to_string(),
                mime_type: "text/plain".to_string(),
                source: echo_agent_app_core::api::types::AttachmentSource::Upload,
            }],
            run_resume: None,
            input_attempt: None,
        };
        let prepared = echo_agent_app_core::api::prepared_turn::PreparedUserTurn {
            instruction: "prepared".to_string(),
            resources: vec![echo_agent_app_core::api::prepared_turn::InputResourceRef {
                path: scoped.clone(),
                name: "input.txt".to_string(),
                mime_type: "text/plain".to_string(),
                kind: echo_agent_app_core::api::prepared_turn::ResourceKind::Document,
                delivery: echo_agent_app_core::api::prepared_turn::Delivery::Inline,
                bytes: 5,
                chars: None,
                lines: None,
                sha256: None,
                source: echo_agent_app_core::api::types::AttachmentSource::Upload,
            }],
            authorship: echo_agent_app_core::api::prepared_turn::InstructionAuthorship::User,
        };

        let retry = request_from_prepared(&request, &prepared);

        assert_eq!(retry.text, "prepared");
        assert_eq!(
            retry.attachments.first().map(|attachment| &attachment.path),
            Some(&scoped)
        );
    }

    #[test]
    fn durable_frontier_is_the_only_tui_queue_projection() -> Result<(), String> {
        let mut app = app();
        app.input = "unsubmitted editor draft".to_string();
        app.cursor = app.input.len();
        let address = ConversationInputAddress {
            workspace_id: "workspace-a".to_string(),
            conversation_id: "conversation-a".to_string(),
        };
        let identity = echo_agent_app_core::api::conversation_input::ConversationInputIdentity {
            address,
            input_id: "input-a".to_string(),
            revision: 1,
            payload_sha256: "payload-a".to_string(),
        };
        app.conversation_input_frontier = Some(
            echo_agent_app_core::api::conversation_input::ConversationInputFrontier {
                queue_revision: 1,
                items: vec![ConversationInputProjection {
                    receipt:
                        echo_agent_app_core::api::conversation_input::ConversationInputReceipt {
                            identity: identity.clone(),
                            phase: ConversationInputPhase::Persisted,
                            attempt: None,
                            attempt_id: None,
                            turn_id: None,
                            outcome: None,
                            drained: false,
                            reason: None,
                            duplicate: false,
                            queue_revision: 1,
                        },
                    payload:
                        echo_agent_app_core::api::conversation_input::ConversationInputPayload {
                            text: "durable next input".to_string(),
                            attachments: Vec::new(),
                            submitted_at_ms: 1,
                            payload_sha256: identity.payload_sha256,
                        },
                    active_attempt: None,
                }],
            },
        );
        let mut second = app
            .conversation_input_frontier
            .as_ref()
            .and_then(|frontier| frontier.items.first())
            .cloned()
            .ok_or_else(|| "first durable projection missing".to_string())?;
        second.receipt.identity.input_id = "input-b".to_string();
        second.payload.text = "durable second input".to_string();
        if let Some(frontier) = app.conversation_input_frontier.as_mut() {
            frontier.items.push(second);
        }

        assert_eq!(app.conversation_input_queue_len(), 2);
        assert_eq!(
            app.next_conversation_input_preview().as_deref(),
            Some("durable next input")
        );
        assert_eq!(app.input, "unsubmitted editor draft");
        assert!(
            !app.messages
                .iter()
                .any(|message| message.content == "durable next input")
        );

        let fact = ConversationInputFact::Persisted {
            identity: app
                .conversation_input_frontier
                .as_ref()
                .and_then(|frontier| frontier.items.first())
                .map(|item| item.receipt.identity.clone())
                .ok_or_else(|| "frontier identity missing".to_string())?,
            payload: app
                .conversation_input_frontier
                .as_ref()
                .and_then(|frontier| frontier.items.first())
                .map(|item| item.payload.clone())
                .ok_or_else(|| "frontier payload missing".to_string())?,
        };
        assert!(format_conversation_input_fact(&fact).contains("Persisted"));
        Ok(())
    }

    #[tokio::test]
    async fn typed_receipt_producer_reaches_render_and_refreshes_frontier()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let log = std::sync::Arc::new(
            echo_agent_app_core::api::chat_event_log::ChatEventLog::open(
                temp.path(),
                echo_agent_app_core::api::chat_event_log::ChatEventRetention::default(),
            )?,
        );
        let service =
            echo_agent_app_core::api::conversation_input::ConversationInputService::new(log);
        let address = ConversationInputAddress {
            workspace_id: "workspace-receipt".to_string(),
            conversation_id: "conversation-receipt".to_string(),
        };
        let receipt = service
            .submit(
                address.clone(),
                "input-receipt".to_string(),
                "refresh from typed receipt".to_string(),
                Vec::new(),
            )
            .await?;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(AgentEvent::ConversationInputReceipt(Box::new(receipt)))?;
        let mut app = app();

        match rx.recv().await {
            Some(AgentEvent::ConversationInputReceipt(receipt)) => {
                render_conversation_input_receipt(&mut app, *receipt, Some(service)).await;
            }
            Some(_) => return Err("unexpected TUI receipt event".into()),
            None => return Err("TUI receipt producer closed before delivery".into()),
        }

        assert!(app.status_msg.contains("Persisted"));
        let frontier = app
            .conversation_input_frontier
            .as_ref()
            .ok_or("receipt render did not refresh the Frontier")?;
        assert_eq!(frontier.items.len(), 1);
        assert_eq!(
            frontier
                .items
                .first()
                .map(|item| item.receipt.identity.address.clone()),
            Some(address)
        );
        Ok(())
    }

    #[tokio::test]
    async fn exact_tui_terminal_projection_survives_stream_cache_eviction()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let log = std::sync::Arc::new(
            echo_agent_app_core::api::chat_event_log::ChatEventLog::open(
                temp.path(),
                echo_agent_app_core::api::chat_event_log::ChatEventRetention::default(),
            )?,
        );
        let service =
            echo_agent_app_core::api::conversation_input::ConversationInputService::new(log);
        let address = ConversationInputAddress {
            workspace_id: "workspace-eviction".to_string(),
            conversation_id: "conversation-target".to_string(),
        };
        let persisted = service
            .submit(
                address.clone(),
                "input-target".to_string(),
                "survive eviction".to_string(),
                Vec::new(),
            )
            .await?;
        let frontier = service.list(&address).await?;
        let started = service
            .dispatch_selected(
                persisted.identity,
                frontier.queue_revision,
                "turn-target".to_string(),
            )
            .await?;
        let attempt = exact_conversation_input_attempt(&started)?;

        for index in 0..130u16 {
            let other = ConversationInputAddress {
                workspace_id: "workspace-eviction".to_string(),
                conversation_id: format!("conversation-{index}"),
            };
            service
                .submit(
                    other,
                    format!("input-{index}"),
                    "cache pressure".to_string(),
                    Vec::new(),
                )
                .await?;
        }

        let receipt = service
            .settle_attempt(&attempt, &TurnOutcome::Completed)
            .await?;
        assert_eq!(receipt.phase, ConversationInputPhase::TurnSettled);
        assert_eq!(receipt.identity.address, address);
        Ok(())
    }

    #[test]
    fn exact_foreground_snapshot_controls_active_idle_routing() {
        let address = ConversationInputAddress {
            workspace_id: "workspace-a".to_string(),
            conversation_id: "conversation-a".to_string(),
        };
        let matching = ForegroundTurnSnapshot {
            workspace_id: address.workspace_id.clone(),
            surface: ForegroundTurnSurface::Tui,
            conversation_id: address.conversation_id.clone(),
            root_turn_id: "root-a".to_string(),
            active_turn_id: "active-a".to_string(),
            cancellation_requested: false,
        };
        let stale = ForegroundTurnSnapshot {
            conversation_id: "conversation-b".to_string(),
            ..matching.clone()
        };

        assert_eq!(
            exact_active_turn_for_address(Some(&matching), &address).as_deref(),
            Some("active-a")
        );
        assert!(exact_active_turn_for_address(Some(&stale), &address).is_none());
        assert!(exact_active_turn_for_address(None, &address).is_none());
    }

    #[test]
    fn live_input_lifecycle_is_core_supervised_and_non_blocking() {
        const SOURCE: &str = include_str!("events.rs");
        let adapter = SOURCE
            .split("async fn steer_conversation_input_projection")
            .nth(1)
            .and_then(|tail| tail.split("enum TurnDispatchResult").next())
            .unwrap_or_default();
        assert!(adapter.contains("steer_input_tracked"));
        assert!(adapter.contains("ConversationInputReceipt"));
        assert!(adapter.contains("supervise_input_lifecycle_scoped"));
        assert!(adapter.contains("observe_steer_through_drain"));
        assert!(adapter.contains("ForegroundTerminalProjector"));
        assert!(adapter.contains(".settle_attempt("));
        let registration = adapter
            .find("supervise_input_lifecycle_scoped")
            .unwrap_or(usize::MAX);
        let effect = adapter.find("steer_input_tracked").unwrap_or(0);
        assert!(registration < effect);
        assert!(!adapter.contains("effect_accepted"));
        assert!(!adapter.contains("wait_for_drained"));
        assert!(!adapter.contains("tokio::spawn"));
    }

    #[tokio::test]
    async fn closed_lifecycle_registration_never_executes_the_steer_effect() -> Result<(), String> {
        let control = echo_agent_app_core::api::foreground_turn::ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-closed",
                ForegroundTurnSurface::Tui,
                "conversation-closed",
                "turn-closed",
            )
            .map_err(|error| error.to_string())?;
        lease
            .settle_after_observers(TurnOutcome::Completed)
            .await
            .map_err(|error| error.to_string())?;
        let projector: echo_agent_app_core::api::foreground_turn::ForegroundTerminalProjector =
            Arc::new(|_| Box::pin(async { Ok(()) }));
        let registration = control.supervise_input_lifecycle_scoped(
            "workspace-closed",
            ForegroundTurnSurface::Tui,
            "conversation-closed",
            "turn-closed",
            async { Ok(()) },
            projector,
        );
        let effects = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_effects = std::sync::Arc::clone(&effects);
        let (handoff, _receiver) = tokio::sync::oneshot::channel();

        let result = execute_registered_tui_steer(
            registration,
            move || async move {
                observed_effects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                "must-not-run"
            },
            handoff,
        )
        .await;

        assert!(matches!(
            result,
            Err(RegisteredTuiSteerError::Registration(_))
        ));
        assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn fast_steer_handoff_keeps_owner_through_terminal_projection() -> Result<(), String> {
        let control = echo_agent_app_core::api::foreground_turn::ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-fast",
                ForegroundTurnSurface::Tui,
                "conversation-fast",
                "turn-fast",
            )
            .map_err(|error| error.to_string())?;
        let waiter = control
            .settlement_waiter_scoped(
                "workspace-fast",
                ForegroundTurnSurface::Tui,
                "conversation-fast",
                "turn-fast",
            )
            .map_err(|error| error.to_string())?;
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_by_owner = std::sync::Arc::clone(&observed);
        let projected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let projected_by_owner = std::sync::Arc::clone(&projected);
        let (handoff, receiver) = tokio::sync::oneshot::channel();
        let projector: echo_agent_app_core::api::foreground_turn::ForegroundTerminalProjector =
            Arc::new(move |outcome| {
                let projected = std::sync::Arc::clone(&projected_by_owner);
                Box::pin(async move {
                    if outcome == TurnOutcome::Completed {
                        projected.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    Ok(())
                })
            });
        let registration = control.supervise_input_lifecycle_scoped(
            "workspace-fast",
            ForegroundTurnSurface::Tui,
            "conversation-fast",
            "turn-fast",
            async move {
                if receiver.await.map_err(|error| error.to_string())? == "accepted" {
                    observed_by_owner.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(())
            },
            projector,
        );

        execute_registered_tui_steer(registration, || async { "accepted" }, handoff)
            .await
            .map_err(|error| format!("steer handoff failed: {error:?}"))?;
        lease
            .settle_after_observers(TurnOutcome::Completed)
            .await
            .map_err(|error| error.to_string())?;

        assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(projected.load(std::sync::atomic::Ordering::SeqCst));
        let settlement = waiter.wait().await.map_err(|error| error.to_string())?;
        assert_eq!(settlement.outcome, TurnOutcome::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn planned_resume_waits_for_registered_live_terminal_projection() -> Result<(), String> {
        let control = echo_agent_app_core::api::foreground_turn::ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-resume",
                ForegroundTurnSurface::Tui,
                "conversation-resume",
                "turn-resume",
            )
            .map_err(|error| error.to_string())?;
        let waiter = control
            .settlement_waiter_scoped(
                "workspace-resume",
                ForegroundTurnSurface::Tui,
                "conversation-resume",
                "turn-resume",
            )
            .map_err(|error| error.to_string())?;
        let (projected, projection_wait) = tokio::sync::oneshot::channel();
        let projector = Arc::new(std::sync::Mutex::new(Some(projected)));
        let terminal_projector: echo_agent_app_core::api::foreground_turn::ForegroundTerminalProjector =
            Arc::new(move |outcome| {
                let projector = Arc::clone(&projector);
                Box::pin(async move {
                    if let Some(sender) = projector
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                    {
                        sender
                            .send(outcome)
                            .map_err(|_| "planned resume projection receiver closed".to_string())?;
                    }
                    Ok(())
                })
            });
        control
            .supervise_input_lifecycle_scoped(
                "workspace-resume",
                ForegroundTurnSurface::Tui,
                "conversation-resume",
                "turn-resume",
                async { Ok(()) },
                terminal_projector,
            )
            .map_err(|error| error.to_string())?;

        let outcome = settle_planned_resume_foreground(lease, TurnOutcome::Completed).await;
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(
            projection_wait.await.map_err(|error| error.to_string())?,
            TurnOutcome::Completed
        );
        let settlement = waiter.wait().await.map_err(|error| error.to_string())?;
        assert_eq!(settlement.outcome, TurnOutcome::Completed);
        Ok(())
    }

    #[test]
    fn initial_input_terminal_is_owned_by_ingress_before_ui_settlement() {
        const SOURCE: &str = include_str!("events.rs");
        let driver = SOURCE
            .split("async fn send_to_agent")
            .nth(1)
            .and_then(|tail| tail.split("fn run_turn_binding_for_request").next())
            .unwrap_or_default();
        assert!(driver.contains("drive_foreground_chat_with_ingress"));
        assert!(driver.contains("observe_turn_input_through_drain"));
        assert!(driver.contains(".settle_attempt("));
        let event_projection = SOURCE
            .split("AgentEvent::TurnSettled")
            .nth(1)
            .and_then(|tail| tail.split("AgentEvent::ExecutionPath").next())
            .unwrap_or_default();
        assert!(!event_projection.contains(".settle_attempt("));
    }

    #[test]
    fn task_run_resume_stays_outside_conversation_input_ingress() {
        const SOURCE: &str = include_str!("events.rs");
        let driver = SOURCE
            .split("async fn send_to_agent")
            .nth(1)
            .and_then(|tail| tail.split("fn run_turn_binding_for_request").next())
            .unwrap_or_default();
        let resume = driver
            .split("if let Some(expected) = planned_resume")
            .nth(1)
            .and_then(|tail| tail.split("let _pool_execution").next())
            .unwrap_or_default();
        assert!(resume.contains("launch_planned_run_resume"));
        assert!(!resume.contains("drive_foreground_chat_with_ingress"));
        assert!(!resume.contains("ConversationInput"));
    }

    #[test]
    fn observer_failure_is_delegated_to_core_failed_terminal() {
        const SOURCE: &str = include_str!("events.rs");
        let adapter = SOURCE
            .split("async fn steer_conversation_input_projection")
            .nth(1)
            .and_then(|tail| tail.split("enum TurnDispatchResult").next())
            .unwrap_or_default();
        assert!(adapter.contains("observe_steer_through_drain"));
        assert!(adapter.contains("supervise_input_lifecycle_scoped"));
        assert!(!adapter.contains("TurnOutcome::Failed"));
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
                artifact: None,
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
        use echo_agent_app_core::api::hitl::TuiHumanLoopProvider;

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
        use echo_agent_app_core::api::hitl::TuiHumanLoopProvider;

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
        pending: &echo_agent_app_core::api::hitl::PendingApprovalQueue,
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
        let worktree =
            echo_agent_app_core::api::tasks::task_runtime::worktree::UnattendedWorktreeInfo {
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
    fn tui_workspace_file_resolution_accepts_repo_files_and_rejects_empty_input()
    -> Result<(), String> {
        let root = std::env::current_dir().map_err(|error| error.to_string())?;
        assert!(resolve_tui_workspace_file(&root, "Cargo.toml").is_ok());
        assert!(resolve_tui_workspace_file(&root, "").is_err());
        Ok(())
    }

    #[test]
    fn task_runtime_projection_formats_plan_state() {
        let view = TaskRuntimeView {
            workspace_id: "workspace-a".to_string(),
            conversation_id: "conversation-a".to_string(),
            run_id: "run-1".to_string(),
            run_created_at: chrono::Utc::now(),
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
                owner_agent: None,
                started_at: None,
                completed_at: None,
                summary: None,
            }],
            completion_ready: true,
            requirements: vec![TaskRuntimeRequirementView {
                requirement_id: "req:tui".to_string(),
                title: "TUI 功能对等".to_string(),
                status: "accepted".to_string(),
            }],
        };

        let text = format_task_runtime_view(&view);
        assert!(text.contains("run-1 [running]"));
        assert!(text.contains("Continuation: active | turn: 4 | compactions: 2"));
        assert!(text.contains("Tokens: 12345 used | 50000 budget | 37655 remaining"));
        assert!(text.contains("Time: 90s used | 300s budget | 210s remaining"));
        assert!(text.contains("[completed] 实现队列 (implementer)"));
        assert!(text.contains("Completion gate: ready"));
        assert!(text.contains("[accepted] req:tui: TUI 功能对等"));
    }

    #[test]
    fn tui_task_projection_preserves_canonical_block_and_metadata() -> Result<(), String> {
        let created_at = chrono::Utc::now();
        let spec = PlanTask {
            id: "child".to_string(),
            title: "依赖失败后暂停".to_string(),
            agent_role: "reviewer".to_string(),
            ..PlanTask::default()
        }
        .spec();
        let plan = PlanRevision {
            plan_id: "plan-1".to_string(),
            run_id: "run-1".to_string(),
            revision: 2,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: "goal-hash".to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![spec],
        };
        let todos = vec![TodoItem {
            id: "todo-child".to_string(),
            run_id: "run-1".to_string(),
            task_id: "child".to_string(),
            title: "依赖失败后暂停".to_string(),
            status: TodoStatus::Blocked,
            retry_count: 1,
            max_retries: 3,
            owner_agent: Some("reviewer".to_string()),
            started_at: Some(created_at),
            completed_at: None,
            summary: Some("blocked by failed ancestor task(s): parent".to_string()),
        }];

        let views = project_tui_task_views(&plan, &todos);
        assert_eq!(views.len(), 1);
        let view = views
            .first()
            .ok_or_else(|| "missing projected TUI task".to_string())?;
        assert_eq!(view.title, "依赖失败后暂停");
        assert_eq!(view.status, "blocked");
        assert_eq!(view.agent_role, "reviewer");
        assert_eq!(view.owner_agent.as_deref(), Some("reviewer"));
        assert_eq!(view.started_at, Some(created_at));
        assert_eq!(
            view.summary.as_deref(),
            Some("blocked by failed ancestor task(s): parent")
        );
        Ok(())
    }

    #[test]
    fn tui_task_projection_follows_plan_revision_order() -> Result<(), String> {
        let make_spec = |id: &str, title: &str| {
            PlanTask {
                id: id.to_string(),
                title: title.to_string(),
                agent_role: "general".to_string(),
                ..PlanTask::default()
            }
            .spec()
        };
        let plan = PlanRevision {
            plan_id: "plan-order".to_string(),
            run_id: "run-order".to_string(),
            revision: 2,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: "goal-hash".to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![make_spec("second", "Second"), make_spec("first", "First")],
        };
        let todos = ["first", "second"]
            .into_iter()
            .map(|task_id| TodoItem {
                id: format!("todo-{task_id}"),
                run_id: "run-order".to_string(),
                task_id: task_id.to_string(),
                title: task_id.to_string(),
                status: TodoStatus::Pending,
                retry_count: 0,
                max_retries: 3,
                owner_agent: None,
                started_at: None,
                completed_at: None,
                summary: None,
            })
            .collect::<Vec<_>>();

        let views = project_tui_task_views(&plan, &todos);
        assert_eq!(
            views
                .iter()
                .map(|view| view.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Second", "First"]
        );
        Ok(())
    }

    #[test]
    fn stale_workspace_view_cannot_select_same_run_id_in_new_workspace() {
        let created_at = chrono::Utc::now();
        let view = TaskRuntimeView {
            workspace_id: "workspace-a".to_string(),
            conversation_id: "conversation-a".to_string(),
            run_id: "same-run".to_string(),
            run_created_at: created_at,
            goal: "goal A".to_string(),
            goal_revision: 1,
            status: "paused".to_string(),
            continuation_enabled: false,
            turn_ordinal: None,
            tokens_used: 0,
            token_budget: None,
            time_used_seconds: 0,
            time_budget_seconds: None,
            compaction_count: 0,
            pause_reason: None,
            pause_detail: None,
            deferred: false,
            active_cell_count: 0,
            tasks: Vec::new(),
            completion_ready: false,
            requirements: Vec::new(),
        };
        let run = echo_agent_app_core::api::tasks::task_runtime::TaskRun {
            run_id: "same-run".to_string(),
            workspace_id: "workspace-b".to_string(),
            conversation_id: "conversation-b".to_string(),
            root_message_id: "root-b".to_string(),
            domain_profile: DomainProfile::General,
            status: TaskRunStatus::Paused,
            goal: "goal B".to_string(),
            goal_revision: 1,
            goal_sha256: echo_agent_app_core::api::tasks::task_runtime::task_goal_sha256("goal B"),
            plan_id: None,
            route: "task".to_string(),
            attended_mode: AttendedMode::Attended,
            attachments: Vec::new(),
            created_at: created_at + chrono::Duration::milliseconds(1),
            updated_at: created_at + chrono::Duration::milliseconds(1),
        };

        assert!(
            validate_tui_task_run_scope(&run, "workspace-b", "conversation-b", Some(&view))
                .is_err_and(|error| error.contains("stale"))
        );
    }

    #[test]
    fn task_runtime_projection_formats_exhausted_time_budget_without_underflow() {
        let view = TaskRuntimeView {
            workspace_id: "workspace-a".to_string(),
            conversation_id: "conversation-a".to_string(),
            run_id: "run-time-budget".to_string(),
            run_created_at: chrono::Utc::now(),
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
            completion_ready: false,
            requirements: Vec::new(),
        };

        let text = format_task_runtime_view(&view);
        assert!(text.contains("Tokens: 900 used | unbounded"));
        assert!(text.contains("Time: 305s used | 300s budget | 0s remaining"));
        assert!(text.contains("Pause reason: time budget exhausted"));
        assert!(text.contains("Pause detail: configured limit reached"));
    }

    #[test]
    fn subagent_events_update_live_projection() {
        use echo_agent::subagent::{ExecutionMode, SubagentEvent};

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
                invocation: echo_agent::agent::ToolInvocation {
                    requested_name: "read_file".to_string(),
                    requested_args: serde_json::json!({}),
                    name: "read_file".to_string(),
                    args: serde_json::json!({}),
                    rewrites: Vec::new(),
                },
                execution_id: Some("task-1:1".to_string()),
                run_id: Some("run-1".to_string()),
            },
        );
        let mut terminal_outcome = echo_agent::subagent::SubagentOutcome {
            contract_version: 2,
            status: echo_agent::subagent::SubagentStatus::Completed,
            summary: "done".to_string(),
            artifacts: vec![echo_agent::subagent::SubagentArtifact {
                path: "report.json".to_string(),
                kind: "report".to_string(),
                bytes: Some(42),
                sha256: Some("a".repeat(64)),
                producer_execution_id: Some("task-1:1".to_string()),
                available: true,
            }],
            evidence: vec![
                echo_agent::subagent::SubagentEvidence {
                    kind: "verification".to_string(),
                    subject: "cargo test".to_string(),
                    outcome: Some("passed".to_string()),
                    details: "ok".to_string(),
                    source: echo_agent::subagent::SubagentEvidenceSource::Observed,
                    attributes: serde_json::Value::Null,
                },
                echo_agent::subagent::SubagentEvidence {
                    kind: "file_read".to_string(),
                    subject: "src/lib.rs".to_string(),
                    outcome: Some("succeeded".to_string()),
                    details: String::new(),
                    source: echo_agent::subagent::SubagentEvidenceSource::Observed,
                    attributes: serde_json::Value::Null,
                },
                echo_agent::subagent::SubagentEvidence {
                    kind: "file_write".to_string(),
                    subject: "report.json".to_string(),
                    outcome: Some("succeeded".to_string()),
                    details: String::new(),
                    source: echo_agent::subagent::SubagentEvidenceSource::Observed,
                    attributes: serde_json::Value::Null,
                },
            ],
            verification: Vec::new(),
            remaining_work: Vec::new(),
            touched_files: echo_agent::subagent::SubagentTouchedFiles::default(),
        };
        terminal_outcome.refresh_derived_views();
        update_subagent_runs(
            &mut app,
            &SubagentEvent::DispatchCompleted {
                parent: "main".to_string(),
                agent: "explorer".to_string(),
                duration_ms: 120,
                tokens_used: Some(42),
                iterations: Some(1),
                output: "done".to_string(),
                outcome: terminal_outcome,
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
