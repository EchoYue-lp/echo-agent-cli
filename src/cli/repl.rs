//! REPL (Read-Eval-Print-Loop) 交互实现
//!
//! 提供交互式命令行界面，支持：
//! - 多行输入
//! - 历史记录
//! - 自动补全
//! - 流式输出显示
//! - 思考步骤可视化
//! - 工具调用交互式审批
//! - Token 用量追踪

use std::collections::VecDeque;
use std::result::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use nu_ansi_term::Color;
use reedline::{Prompt, PromptHistorySearchStatus, Signal};

use crate::agent_handle::AgentHandle;
use echo_agent::prelude::*;

use super::commands::{CommandHandler, CommandResult};
use super::editor::{EditorConfig, create_enhanced_editor};
use crate::output::OutputRenderer;

static TOTAL_INPUT_TOKENS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_OUTPUT_TOKENS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_TOOL_CALLS: AtomicUsize = AtomicUsize::new(0);
static FILE_CHANGE_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone)]
struct ExplicitTaskRunResume {
    run_id: String,
    root_message_id: String,
}

struct QueuedReplTurn {
    message: String,
    interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode,
    attachments: Vec<echo_agent_app_core::attachments::AttachmentRef>,
    task_run_resume: Option<ExplicitTaskRunResume>,
}

#[derive(Default)]
struct ReplTurnQueue {
    turns: VecDeque<QueuedReplTurn>,
}

impl ReplTurnQueue {
    fn enqueue(&mut self, turn: QueuedReplTurn) {
        self.turns.push_back(turn);
    }

    fn len(&self) -> usize {
        self.turns.len()
    }

    fn front_for_idle(&self, has_active_turn: bool) -> Option<&QueuedReplTurn> {
        if has_active_turn {
            None
        } else {
            self.turns.front()
        }
    }

    fn consume_front(&mut self) -> Option<QueuedReplTurn> {
        self.turns.pop_front()
    }

    fn discard_attachments(&mut self) -> Vec<echo_agent_app_core::attachments::AttachmentRef> {
        self.turns
            .drain(..)
            .flat_map(|turn| turn.attachments)
            .collect()
    }

    fn settle_start_failure(
        &mut self,
        error: &ReplTurnStartError,
    ) -> QueuedStartFailureDisposition {
        if error.should_retain_fifo_head() {
            QueuedStartFailureDisposition::Retained
        } else {
            let _ = self.consume_front();
            QueuedStartFailureDisposition::Consumed
        }
    }
}

fn enqueue_idle_input(queued: &mut ReplTurnQueue, input: QueuedReplTurn) {
    queued.enqueue(input);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuedStartFailureDisposition {
    Retained,
    Consumed,
}

enum ReplTurnStartError {
    Retryable(echo_agent_app_core::foreground_turn::ForegroundTurnError),
    Permanent(String),
}

impl ReplTurnStartError {
    fn from_admission(error: echo_agent_app_core::foreground_turn::ForegroundTurnError) -> Self {
        match error {
            error @ (echo_agent_app_core::foreground_turn::ForegroundTurnError::Busy { .. }
            | echo_agent_app_core::foreground_turn::ForegroundTurnError::AdmissionSuspended) => {
                Self::Retryable(error)
            }
            error => Self::Permanent(error.to_string()),
        }
    }

    fn from_conversation_admission(
        error: echo_agent_app_core::conversation_deletion::ConversationDeletionError,
    ) -> Self {
        match error {
            echo_agent_app_core::conversation_deletion::ConversationDeletionError::Foreground(
                error,
            ) => Self::from_admission(error),
            error => Self::Permanent(error.to_string()),
        }
    }

    fn from_scoped_admission(error: echo_agent_app_core::state::ScopedChatTurnError) -> Self {
        match error {
            echo_agent_app_core::state::ScopedChatTurnError::Conversation(error) => {
                Self::from_conversation_admission(error)
            }
            echo_agent_app_core::state::ScopedChatTurnError::Runtime(error) => {
                Self::Permanent(error)
            }
        }
    }

    fn should_retain_fifo_head(&self) -> bool {
        matches!(self, Self::Retryable(_))
    }

    fn message(&self) -> String {
        match self {
            Self::Retryable(error) => error.to_string(),
            Self::Permanent(error) => error.clone(),
        }
    }
}

struct PreparedReplTurnStart {
    scoped_runtime: echo_agent_app_core::state::ScopedChatRuntime,
    pool_execution: echo_agent_app_core::agent_pool::AgentPoolExecutionLease,
    conversation_id: String,
    turn_id: String,
    turn: echo_agent_app_core::prepared_turn::PreparedUserTurn,
    control: echo_agent_app_core::foreground_turn::ForegroundTurnControl,
    lease: echo_agent_app_core::foreground_turn::ForegroundTurnLease,
}

struct ActiveReplTurn {
    workspace_id: String,
    conversation_id: String,
    turn_id: String,
    control: echo_agent_app_core::foreground_turn::ForegroundTurnControl,
    task: Option<tokio::task::JoinHandle<usize>>,
    completion: Option<tokio::sync::oneshot::Receiver<()>>,
    cancel_on_drop: bool,
}

impl ActiveReplTurn {
    fn is_finished(&self) -> bool {
        self.task
            .as_ref()
            .map(tokio::task::JoinHandle::is_finished)
            .unwrap_or(true)
    }
}

impl Drop for ActiveReplTurn {
    fn drop(&mut self) {
        if !self.cancel_on_drop {
            return;
        }
        if self
            .task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            return;
        }
        if let Err(error) = self.control.request_cancel_scoped(
            &self.workspace_id,
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
            &self.conversation_id,
            &self.turn_id,
        ) {
            tracing::debug!(%error, "dropped CLI turn could not request exact cancellation");
        }
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
        // Drop cannot await. Normal REPL exits use cancel_and_drain_active;
        // this defensive boundary cancels the exact owner before aborting its
        // sole supervisor. Dropping the supervisor drops its lease, which
        // settles the foreground registry without detaching an inner task.
    }
}

struct PendingGitAction {
    changes: usize,
}

#[derive(Clone)]
struct ReplExternalOutput {
    sender: Arc<dyn Fn(String) -> bool + Send + Sync>,
    token_buffer: Arc<std::sync::Mutex<ReplTokenBuffer>>,
    delivery_failed: Arc<AtomicBool>,
    turn_cancel: Arc<std::sync::Mutex<Option<tokio_util::sync::CancellationToken>>>,
}

/// Owned REPL HITL registration and input-broker channels.
///
/// CLI startup creates this before headless services so scheduler/bootstrap
/// requests can never observe an empty dispatcher. The same owner is moved
/// into [`run_repl`] and closes admission, rejects exact pending requests, and
/// unregisters the provider on every exit path.
pub struct ReplHumanLoopSession {
    registration: Option<echo_agent_app_core::hitl::HitlProviderRegistration>,
    provider: Option<Arc<echo_agent_app_core::hitl::ReplHumanLoopProvider>>,
    external_printer: Option<reedline::ExternalPrinter<String>>,
    live_output: ReplExternalOutput,
    request_rx: tokio::sync::mpsc::UnboundedReceiver<
        echo_agent_app_core::hitl::PendingReplHumanLoopRequest,
    >,
    failure_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
}

impl ReplHumanLoopSession {
    pub async fn register(dispatcher: Arc<echo_agent_app_core::hitl::HitlDispatcher>) -> Self {
        let external_printer = reedline::ExternalPrinter::new(4096);
        let external_sender = external_printer.sender();
        let live_output =
            ReplExternalOutput::new(move |message| external_sender.try_send(message).is_ok());
        let prompt_output = live_output.clone();
        let (provider, request_rx, failure_rx) =
            echo_agent_app_core::hitl::ReplHumanLoopProvider::channel(Arc::new(move |prompt| {
                if prompt_output.emit(prompt) {
                    Ok(())
                } else {
                    Err("external printer receiver closed or queue full".to_string())
                }
            }));
        let provider = Arc::new(provider);
        let registration = dispatcher.register_owned("repl", provider.clone()).await;
        Self {
            registration: Some(registration),
            provider: Some(provider),
            external_printer: Some(external_printer),
            live_output,
            request_rx,
            failure_rx,
        }
    }

    pub async fn shutdown(mut self, reason: &str) -> anyhow::Result<()> {
        if let Some(registration) = self.registration.take() {
            registration.unregister();
        }
        let close_result = self
            .provider
            .take()
            .ok_or_else(|| anyhow::anyhow!("REPL HITL provider owner is unavailable"))?
            .close(reason)
            .map_err(anyhow::Error::from);
        while let Ok(request) = self.request_rx.try_recv() {
            if !request.is_expired() {
                let _ = request.reject(reason.to_string());
            }
        }
        close_result.map(|_| ())
    }
}

impl Drop for ReplHumanLoopSession {
    fn drop(&mut self) {
        if let Some(registration) = self.registration.take() {
            registration.unregister();
        }
        if let Some(provider) = self.provider.take() {
            let _ = provider.close("REPL HITL session owner dropped");
        }
    }
}

struct ReplTokenBuffer {
    text: String,
    last_flush: std::time::Instant,
}

impl ReplExternalOutput {
    fn new(sender: impl Fn(String) -> bool + Send + Sync + 'static) -> Self {
        Self {
            sender: Arc::new(sender),
            token_buffer: Arc::new(std::sync::Mutex::new(ReplTokenBuffer {
                text: String::new(),
                last_flush: std::time::Instant::now(),
            })),
            delivery_failed: Arc::new(AtomicBool::new(false)),
            turn_cancel: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn bind_turn_cancel(&self, cancel: tokio_util::sync::CancellationToken) -> bool {
        let bound = match self.turn_cancel.lock() {
            Ok(mut slot) => {
                *slot = Some(cancel.clone());
                true
            }
            Err(error) => {
                tracing::warn!(%error, "REPL output cancellation slot is unavailable");
                false
            }
        };
        if !bound || self.delivery_failed() {
            cancel.cancel();
            return false;
        }
        true
    }

    fn clear_turn_cancel(&self) {
        match self.turn_cancel.lock() {
            Ok(mut slot) => {
                slot.take();
            }
            Err(error) => {
                tracing::warn!(%error, "REPL output cancellation slot is unavailable");
            }
        }
    }

    fn delivery_failed(&self) -> bool {
        self.delivery_failed.load(Ordering::Acquire)
    }

    fn fail_delivery(&self, reason: &str) -> bool {
        if !self.delivery_failed.swap(true, Ordering::AcqRel) {
            tracing::warn!(%reason, "REPL external output delivery failed");
        }
        match self.turn_cancel.lock() {
            Ok(slot) => {
                if let Some(cancel) = slot.as_ref() {
                    cancel.cancel();
                }
            }
            Err(error) => {
                tracing::warn!(%error, "REPL output cancellation slot is unavailable");
            }
        }
        false
    }

    fn emit(&self, message: impl Into<String>) -> bool {
        self.flush_tokens() && self.send(message.into())
    }

    fn send(&self, message: String) -> bool {
        if self.delivery_failed() {
            return false;
        }
        if (self.sender)(message) {
            true
        } else {
            self.fail_delivery("external printer receiver closed or queue full")
        }
    }

    fn print_user_message(&self, message: &str) -> bool {
        self.emit(format!("You: {message}"))
    }

    fn print_assistant_prefix(&self) -> bool {
        self.print_token("Assistant: ")
    }

    fn print_token(&self, token: &str) -> bool {
        if self.delivery_failed() {
            return false;
        }
        let ready = match self.token_buffer.lock() {
            Ok(mut buffer) => {
                buffer.text.push_str(token);
                buffer.text.contains('\n')
                    || buffer.text.chars().count() >= 160
                    || buffer.last_flush.elapsed() >= std::time::Duration::from_millis(40)
            }
            Err(error) => {
                tracing::warn!(%error, "REPL token buffer is unavailable");
                return self.fail_delivery("token buffer lock poisoned");
            }
        };
        if ready { self.flush_tokens() } else { true }
    }

    fn flush_tokens(&self) -> bool {
        if self.delivery_failed() {
            return false;
        }
        let pending = match self.token_buffer.lock() {
            Ok(mut buffer) => {
                if buffer.text.is_empty() {
                    return true;
                }
                buffer.last_flush = std::time::Instant::now();
                std::mem::take(&mut buffer.text)
            }
            Err(error) => {
                tracing::warn!(%error, "REPL token buffer is unavailable");
                return self.fail_delivery("token buffer lock poisoned");
            }
        };
        self.send(pending)
    }

    fn print_tool_call(&self, name: &str, args: &serde_json::Value) -> bool {
        let args = serde_json::to_string(args).unwrap_or_default();
        let preview: String = args.chars().take(200).collect();
        let suffix = if args.chars().count() > 200 {
            "..."
        } else {
            ""
        };
        self.emit(format!("Tool call {name}: {preview}{suffix}"))
    }

    fn print_tool_result(&self, name: &str, result: &str) -> bool {
        let preview: String = result.chars().take(300).collect();
        let suffix = if result.chars().count() > 300 {
            "..."
        } else {
            ""
        };
        self.emit(format!("Tool result {name}: {preview}{suffix}"))
    }

    fn print_warning(&self, message: &str) -> bool {
        self.emit(format!("Warning: {message}"))
    }

    fn print_error(&self, message: &str) -> bool {
        self.emit(format!("Error: {message}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplRenderedTerminal {
    Completed,
    Cancelled,
    Failed,
}

struct ReplRenderState {
    first_chunk: bool,
    tool_call_count: u32,
    iteration_count: u32,
    started: std::time::Instant,
    terminal: Option<ReplRenderedTerminal>,
}

impl Default for ReplRenderState {
    fn default() -> Self {
        Self {
            first_chunk: true,
            tool_call_count: 0,
            iteration_count: 0,
            started: std::time::Instant::now(),
            terminal: None,
        }
    }
}

struct ReplChatSink {
    output: ReplExternalOutput,
    config: crate::output::OutputConfig,
    state: std::sync::Mutex<ReplRenderState>,
}

impl ReplChatSink {
    fn new(output: ReplExternalOutput, config: crate::output::OutputConfig) -> Self {
        Self {
            output,
            config,
            state: std::sync::Mutex::new(ReplRenderState::default()),
        }
    }

    fn render_agent_event(&self, state: &mut ReplRenderState, event: AgentEvent) -> bool {
        match event {
            AgentEvent::ThinkStart => {
                if !self.output.flush_tokens() {
                    return false;
                }
                state.iteration_count = state.iteration_count.saturating_add(1);
                let label = format!("  ⏳ 思考中 (步骤 {})...", state.iteration_count);
                state.first_chunk = true;
                self.output
                    .emit(nu_ansi_term::Color::Fixed(8).paint(label).to_string())
            }
            AgentEvent::ThinkEnd {
                prompt_tokens,
                completion_tokens,
            } => {
                TOTAL_INPUT_TOKENS.fetch_add(prompt_tokens, Ordering::Relaxed);
                TOTAL_OUTPUT_TOKENS.fetch_add(completion_tokens, Ordering::Relaxed);
                true
            }
            AgentEvent::LlmUsage { .. } => true,
            AgentEvent::Token(token) => {
                if state.first_chunk {
                    state.first_chunk = false;
                    if !self.output.print_assistant_prefix() {
                        return false;
                    }
                }
                self.output.print_token(&token)
            }
            AgentEvent::SafetyNotice {
                action,
                reason,
                risk,
                permission,
            } => {
                state.first_chunk = true;
                let icon = nu_ansi_term::Color::Yellow.paint("Safety");
                self.output.emit(format!(
                    "{icon} {action}\nReason: {reason}\nRisk: {risk} | Permission: {permission}"
                ))
            }
            AgentEvent::ParameterError {
                tool,
                parameter,
                expected,
                got,
            } => {
                state.first_chunk = true;
                let icon = nu_ansi_term::Color::Red.paint("ParamError");
                self.output.emit(format!(
                    "{icon} {tool}: parameter '{parameter}' expected {expected}, got {got}"
                ))
            }
            AgentEvent::BudgetDecision {
                decision,
                reason,
                iteration,
                ..
            } => {
                state.first_chunk = true;
                self.output.emit(format!(
                    "Budget {decision:?} at iteration {iteration}: {reason}"
                ))
            }
            AgentEvent::GuardTriggered { guard, blocked } => {
                state.first_chunk = true;
                self.output
                    .emit(format!("Guard {guard} triggered (blocked={blocked})"))
            }
            AgentEvent::MemoryRecalled { count } => {
                state.first_chunk = true;
                self.output.emit(format!("Recalled {count} memory item(s)"))
            }
            AgentEvent::Chart { spec } => {
                state.first_chunk = true;
                let preview: String = spec.to_string().chars().take(500).collect();
                self.output.emit(format!("Chart specification: {preview}"))
            }
            AgentEvent::ToolCall { invocation, .. } => {
                state.first_chunk = true;
                state.tool_call_count = state.tool_call_count.saturating_add(1);
                TOTAL_TOOL_CALLS.fetch_add(1, Ordering::Relaxed);
                if matches!(
                    invocation.name.as_str(),
                    "shell" | "delete_file" | "git_commit"
                ) {
                    let warning = nu_ansi_term::Color::Red.paint(format!(
                        "DANGER: {} — irreversible operation",
                        invocation.name
                    ));
                    if !self.output.emit(warning.to_string()) {
                        return false;
                    }
                    if invocation.name == "shell"
                        && let Some(command) = invocation
                            .args
                            .get("command")
                            .and_then(serde_json::Value::as_str)
                        && !self.output.emit(format!("Command: {command}"))
                    {
                        return false;
                    }
                }
                self.output
                    .print_tool_call(&invocation.name, &invocation.args)
            }
            AgentEvent::ToolStream {
                event: echo_agent::tools::ToolStreamEvent::Complete(result),
                ..
            } => {
                let Some(path) = result.metadata.get("artifact_path") else {
                    return true;
                };
                let status = if std::path::Path::new(path).is_file() {
                    "Full output artifact"
                } else {
                    "Full output artifact missing"
                };
                let size = result
                    .metadata
                    .get("artifact_bytes")
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|bytes| format!(" ({:.1} MiB)", bytes as f64 / 1_048_576.0))
                    .unwrap_or_default();
                self.output.emit(format!("{status}{size}: {path}"))
            }
            AgentEvent::ToolStream { .. } => true,
            AgentEvent::ToolResult { name, result, .. } => {
                if name == "apply_patch" {
                    FILE_CHANGE_COUNT.fetch_add(1, Ordering::Relaxed);
                }
                state.first_chunk = true;
                if result.success {
                    self.output.print_tool_result(&name, &result.output)
                } else {
                    let error = result.error.as_deref().unwrap_or(&result.output);
                    let detail = result.failure.as_ref().map_or_else(
                        || format!("✗ {name}: {error}"),
                        |failure| {
                            format!(
                                "✗ {name} [{} → {}]: {error}",
                                failure.category.as_str(),
                                failure.recovery.as_str()
                            )
                        },
                    );
                    self.output
                        .emit(nu_ansi_term::Color::Red.paint(detail).to_string())
                }
            }
            AgentEvent::FinalAnswer(_) => {
                let accepted = self.output.flush_tokens();
                if accepted {
                    state.terminal = Some(ReplRenderedTerminal::Completed);
                }
                accepted
            }
            AgentEvent::Cancelled => {
                let accepted = self.output.print_warning("执行已取消");
                if accepted {
                    state.terminal = Some(ReplRenderedTerminal::Cancelled);
                }
                accepted
            }
            AgentEvent::Error {
                source,
                message,
                failure,
            } => {
                let cancelled =
                    failure.terminal_kind == echo_agent::error::AgentTerminalKind::Cancelled;
                let accepted = if cancelled {
                    self.output.print_warning(&format!("[{source}] {message}"))
                } else {
                    self.output.print_error(&format!("[{source}] {message}"))
                };
                if accepted {
                    state.terminal = Some(if cancelled {
                        ReplRenderedTerminal::Cancelled
                    } else {
                        ReplRenderedTerminal::Failed
                    });
                }
                accepted
            }
            AgentEvent::ContextCompressed {
                before_count,
                after_count,
                before_tokens,
                after_tokens,
            } => {
                let saved = before_tokens.saturating_sub(after_tokens);
                let text = format!(
                    "  📦 上下文自动压缩: {before_count}→{after_count} 条消息, \
                     {before_tokens}→{after_tokens} tokens (节省 {saved})"
                );
                self.output
                    .emit(nu_ansi_term::Color::Fixed(8).paint(text).to_string())
            }
            other => {
                tracing::debug!(event = ?other, "CLI received unrecognized future agent event");
                true
            }
        }
    }

    fn project_outcome(
        &self,
        state: &mut ReplRenderState,
        result: &Result<echo_agent_app_core::chat_driver::TurnOutcome, String>,
    ) {
        use echo_agent_app_core::chat_driver::TurnOutcome;

        match result {
            Ok(TurnOutcome::Completed) => {}
            Ok(TurnOutcome::Cancelled) => {
                if state.terminal != Some(ReplRenderedTerminal::Cancelled)
                    && self.output.print_warning("执行已取消")
                {
                    state.terminal = Some(ReplRenderedTerminal::Cancelled);
                }
            }
            Ok(TurnOutcome::Failed(failure)) => {
                tracing::warn!(
                    code = %failure.code,
                    message = %failure.message,
                    "CLI shared chat turn failed"
                );
                if state.terminal != Some(ReplRenderedTerminal::Failed)
                    && self.output.print_error(&format!(
                        "Turn failed [{}]: {}",
                        failure.code, failure.message
                    ))
                {
                    state.terminal = Some(ReplRenderedTerminal::Failed);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "CLI shared chat driver returned an error");
                if state.terminal != Some(ReplRenderedTerminal::Failed)
                    && self
                        .output
                        .print_error(&format!("Chat driver failed: {error}"))
                {
                    state.terminal = Some(ReplRenderedTerminal::Failed);
                }
            }
        }
    }

    fn finish(
        &self,
        result: &Result<echo_agent_app_core::chat_driver::TurnOutcome, String>,
    ) -> usize {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(%error, "REPL renderer state is unavailable");
                self.output.fail_delivery("renderer state lock poisoned");
                FILE_CHANGE_COUNT.store(0, Ordering::Relaxed);
                return 0;
            }
        };
        let _ = self.output.flush_tokens();
        self.project_outcome(&mut state, result);

        let elapsed = state.started.elapsed();
        if self.config.show_token_stats || self.config.show_tool_details {
            let duration = if elapsed.as_secs() >= 60 {
                format!("{}m {}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
            } else {
                format!("{:.1}s", elapsed.as_secs_f64())
            };
            let stats = format!("  ⏱ {duration}  🔧 {} 工具调用", state.tool_call_count);
            let _ = self
                .output
                .emit(nu_ansi_term::Color::Fixed(8).paint(stats).to_string());
        }
        if state.tool_call_count > 0 {
            let _ = self.output.emit(
                nu_ansi_term::Color::Fixed(8)
                    .paint("  Tip: /trace to inspect, /test to verify, /diff to review")
                    .to_string(),
            );
        }

        let changes = FILE_CHANGE_COUNT.swap(0, Ordering::Relaxed);
        if changes > 0 && std::env::current_dir().is_ok_and(|cwd| cwd.join(".git").exists()) {
            let _ = self.output.emit(format!(
                "{changes} file(s) changed: [c] commit  [s] stage  [n] skip"
            ));
            changes
        } else {
            0
        }
    }
}

impl echo_agent_app_core::chat_driver::ChatSink for ReplChatSink {
    fn on_event(&self, event: echo_agent_app_core::chat_driver::ChatDriverEvent) -> bool {
        if self.output.delivery_failed() {
            return false;
        }
        if let Some(projection) =
            echo_agent_app_core::tasks::task_runtime::project_awaiter_surface_event(&event)
        {
            return self.output.emit(projection.display_message());
        }
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(%error, "REPL renderer state is unavailable");
                return self.output.fail_delivery("renderer state lock poisoned");
            }
        };
        match event {
            echo_agent_app_core::chat_driver::ChatDriverEvent::Agent(envelope) => {
                self.render_agent_event(&mut state, envelope.payload)
            }
            echo_agent_app_core::chat_driver::ChatDriverEvent::Execution(event) => {
                let detail: String = event.payload.to_string().chars().take(500).collect();
                self.output.emit(format!(
                    "TaskRuntime {} [{}]: {detail}",
                    event.event, event.run_id
                ))
            }
            echo_agent_app_core::chat_driver::ChatDriverEvent::TurnStatus { status } => {
                status == "running" || self.output.emit(format!("Turn status: {status}"))
            }
            echo_agent_app_core::chat_driver::ChatDriverEvent::ExecutionPath {
                requested_mode,
                observed_path,
            } => self.output.emit(format!(
                "Execution path: {requested_mode} -> {observed_path}"
            )),
            echo_agent_app_core::chat_driver::ChatDriverEvent::TurnConfiguration {
                interaction_mode,
                permission_mode,
                approval_policy,
                attachments,
            } => self.output.emit(format!(
                "Turn configuration: mode={interaction_mode}, permission={permission_mode}, \
                 approval={approval_policy}, attachments={}",
                attachments.len()
            )),
            echo_agent_app_core::chat_driver::ChatDriverEvent::Interrupt {
                run_id,
                goal,
                new_message,
            } => self.output.emit(format!(
                "Run {run_id} paused ({goal}); new instruction: {new_message}"
            )),
            echo_agent_app_core::chat_driver::ChatDriverEvent::InputQueued { input_id, .. } => {
                self.output.emit(format!("Input queued: {input_id}"))
            }
            echo_agent_app_core::chat_driver::ChatDriverEvent::InputRemoved { input_id } => self
                .output
                .emit(format!("Queued input removed: {input_id}")),
            echo_agent_app_core::chat_driver::ChatDriverEvent::InputReordered { .. } => true,
            echo_agent_app_core::chat_driver::ChatDriverEvent::ApprovalRequest {
                request_id,
                tool_name,
                prompt,
                ..
            } => self.output.emit(format!(
                "Approval requested [{request_id}] for {tool_name}: {prompt}"
            )),
            echo_agent_app_core::chat_driver::ChatDriverEvent::InputRequest {
                request_id,
                prompt,
            } => self
                .output
                .emit(format!("Input requested [{request_id}]: {prompt}")),
            echo_agent_app_core::chat_driver::ChatDriverEvent::SelectionRequest {
                request_id,
                prompt,
                options,
                ..
            } => self.output.emit(format!(
                "Selection requested [{request_id}]: {prompt} ({})",
                options.join(", ")
            )),
            echo_agent_app_core::chat_driver::ChatDriverEvent::CommandCellStarted { cell } => {
                self.output.emit(format!(
                    "Command cell {} started: {}",
                    cell.cell_id, cell.name
                ))
            }
            echo_agent_app_core::chat_driver::ChatDriverEvent::CommandCellSettled { cell } => {
                self.output.emit(format!(
                    "Command cell {} settled: {}",
                    cell.cell_id, cell.phase
                ))
            }
            echo_agent_app_core::chat_driver::ChatDriverEvent::AwaiterResultReady { .. }
            | echo_agent_app_core::chat_driver::ChatDriverEvent::AwaiterResultAcknowledged {
                ..
            } => true,
            echo_agent_app_core::chat_driver::ChatDriverEvent::ContextCompressed {
                before_count,
                after_count,
                before_tokens,
                after_tokens,
            } => {
                let saved = before_tokens.saturating_sub(after_tokens);
                self.output.emit(format!(
                    "Context compressed: {before_count}->{after_count} messages, \
                     {before_tokens}->{after_tokens} tokens ({saved} saved)"
                ))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplLineTarget {
    Exit,
    HumanLoop,
    ActiveTurn,
    GitAction,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum QueuedFollowUpWait {
    Settled,
    Exit,
    Interrupted,
    InputClosed,
    ReadError(String),
    SessionFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveInputDisposition {
    Steered,
    Queued,
}

fn line_target(
    line: &str,
    has_pending_hitl: bool,
    has_active_turn: bool,
    has_git: bool,
) -> ReplLineTarget {
    if matches!(line, "/exit" | "/quit" | "/q") {
        ReplLineTarget::Exit
    } else if has_pending_hitl {
        ReplLineTarget::HumanLoop
    } else if has_active_turn && !line.is_empty() {
        ReplLineTarget::ActiveTurn
    } else if has_git {
        ReplLineTarget::GitAction
    } else {
        ReplLineTarget::Idle
    }
}

fn settle_steer_attempt(
    result: Result<String, echo_agent::agent::TurnSteerError>,
    input: QueuedReplTurn,
    queued: &mut ReplTurnQueue,
) -> Result<String, echo_agent::agent::TurnSteerError> {
    match result {
        Ok(turn_id) => Ok(turn_id),
        Err(error) => {
            queued.enqueue(input);
            Err(error)
        }
    }
}

async fn wait_for_queued_follow_up<F, I>(
    active: &mut ActiveReplTurn,
    hitl_rx: &mut tokio::sync::mpsc::UnboundedReceiver<
        echo_agent_app_core::hitl::PendingReplHumanLoopRequest,
    >,
    failure_rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    pending_hitl: &mut VecDeque<echo_agent_app_core::hitl::PendingReplHumanLoopRequest>,
    output: &OutputRenderer,
    mut read_signal: F,
    interrupt: I,
) -> QueuedFollowUpWait
where
    F: FnMut() -> std::io::Result<Signal>,
    I: std::future::Future<Output = ()>,
{
    enum Wake {
        Hitl(Box<Option<echo_agent_app_core::hitl::PendingReplHumanLoopRequest>>),
        SessionFailed(Option<String>),
        Interrupted,
        Settled,
    }
    tokio::pin!(interrupt);

    loop {
        if let Ok(error) = failure_rx.try_recv() {
            return QueuedFollowUpWait::SessionFailed(error);
        }
        drain_pending_hitl(hitl_rx, pending_hitl);
        discard_expired_hitl(pending_hitl);
        if !pending_hitl.is_empty() {
            match read_signal() {
                Ok(Signal::Success(line)) => {
                    let line = line.trim();
                    if matches!(line, "/exit" | "/quit" | "/q") {
                        return QueuedFollowUpWait::Exit;
                    }
                    let _ = resolve_front_hitl(pending_hitl, line, output);
                }
                Ok(Signal::CtrlC) => return QueuedFollowUpWait::Interrupted,
                Ok(Signal::CtrlD) => return QueuedFollowUpWait::InputClosed,
                Err(error) => return QueuedFollowUpWait::ReadError(error.to_string()),
            }
            continue;
        }
        if active.is_finished() {
            return QueuedFollowUpWait::Settled;
        }

        let wake = match active.completion.as_mut() {
            Some(completion) => {
                tokio::select! {
                    biased;
                    _ = &mut interrupt => Wake::Interrupted,
                    failure = failure_rx.recv() => Wake::SessionFailed(failure),
                    request = hitl_rx.recv() => Wake::Hitl(Box::new(request)),
                    _ = &mut *completion => Wake::Settled,
                }
            }
            None => Wake::Settled,
        };
        match wake {
            Wake::Hitl(request) => match *request {
                Some(request) => pending_hitl.push_back(request),
                None => return QueuedFollowUpWait::Settled,
            },
            Wake::SessionFailed(Some(error)) => {
                return QueuedFollowUpWait::SessionFailed(error);
            }
            Wake::Interrupted => return QueuedFollowUpWait::Interrupted,
            Wake::SessionFailed(None) | Wake::Settled => {
                return QueuedFollowUpWait::Settled;
            }
        }
    }
}

pub fn get_usage_stats() -> (usize, usize, usize) {
    (
        TOTAL_INPUT_TOKENS.load(Ordering::Relaxed),
        TOTAL_OUTPUT_TOKENS.load(Ordering::Relaxed),
        TOTAL_TOOL_CALLS.load(Ordering::Relaxed),
    )
}

pub fn reset_usage_stats() {
    TOTAL_INPUT_TOKENS.store(0, Ordering::Relaxed);
    TOTAL_OUTPUT_TOKENS.store(0, Ordering::Relaxed);
    TOTAL_TOOL_CALLS.store(0, Ordering::Relaxed);
    FILE_CHANGE_COUNT.store(0, Ordering::Relaxed);
}

/// REPL 配置
pub struct ReplConfig {
    pub prompt: String,
    pub history_file: String,
    pub mode: String,
    pub project: Option<String>,
    pub task_service: Option<Arc<echo_agent_app_core::tasks::BackgroundTaskService>>,
    pub scheduler_runner: Option<Arc<echo_agent_app_core::scheduler::SchedulerRunner>>,
    /// Shared live plugin runtime from bootstrap.
    pub plugin_runtime: Option<Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>>,
    /// Shared ReviewIntegration from bootstrap. It is the sole Memory binding
    /// authority for Dreaming, chat turns, auto-memory, and session-end review.
    pub review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    /// Static prompt-module report captured during runtime bootstrap.
    pub prompt_assembly: Option<echo_agent_app_core::project::prompt::PromptAssembly>,
    /// Shared pool used by `create_complex_task` and background TaskRuntime runs.
    pub pool: Option<Arc<echo_agent_app_core::agent_pool::AgentPool>>,
    /// Canonical TaskRuntime store shared with TUI/channel/GUI entry points.
    pub task_runtime_store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    /// Persisted conversation identity for the shared chat driver.
    pub conversation_id: String,
    /// Shared webhook emitter (built from `AppConfig.webhooks` at bootstrap).
    /// `None` means no endpoints configured — emit calls are skipped cheaply.
    pub webhook_emitter: Option<std::sync::Arc<echo_agent_app_core::webhook::WebhookEmitter>>,
    /// Authoritative application state used by workspace and other stateful commands.
    pub app_state: Option<Arc<echo_agent_app_core::state::AppState>>,
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            prompt: "echo".to_string(),
            history_file: echo_agent::paths::user_data_path("history.txt")
                .to_string_lossy()
                .into_owned(),
            mode: "general".to_string(),
            project: None,
            task_service: None,
            scheduler_runner: None,
            plugin_runtime: None,
            review_integration: None,
            prompt_assembly: None,
            pool: None,
            task_runtime_store: None,
            conversation_id: uuid::Uuid::new_v4().to_string(),
            webhook_emitter: None,
            app_state: None,
        }
    }
}

/// Run the REPL under its pre-registered HITL session owner.
pub async fn run_repl(
    agent: AgentHandle,
    config: ReplConfig,
    mut hitl_session: ReplHumanLoopSession,
) -> anyhow::Result<()> {
    let result = run_repl_inner(agent, config, &mut hitl_session).await;
    let shutdown_reason = match &result {
        Ok(()) => "CLI session exited",
        Err(_) => "CLI session failed",
    };
    let shutdown = hitl_session.shutdown(shutdown_reason).await;
    match (result, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(shutdown_error)) => Err(anyhow::anyhow!(
            "{error}; REPL HITL shutdown failed: {shutdown_error}"
        )),
    }
}

async fn run_repl_inner(
    agent: AgentHandle,
    config: ReplConfig,
    hitl_session: &mut ReplHumanLoopSession,
) -> anyhow::Result<()> {
    let output = Arc::new(OutputRenderer::default());
    let app_state = config
        .app_state
        .clone()
        .ok_or_else(|| anyhow::anyhow!("CLI REPL requires the shared application state"))?;

    output.print_banner(env!("CARGO_PKG_VERSION"));

    let model_name = agent.read(|a| a.model_name().to_string()).await;

    // Load project context: use explicit --project path, or auto-discover from cwd.
    let project_ctx = {
        let project_path = config.project.as_deref().unwrap_or(".");
        let explicit = config.project.is_some();
        let root = if explicit {
            Some(std::path::PathBuf::from(project_path))
        } else {
            crate::project::context::discover_project_root(Some(std::path::Path::new(".")))
        };
        root.map(|r| crate::project::context::load_project_context(&r))
    };
    // Instruction files are owned by InstructionProvider (single authority).
    let instructions_count = project_ctx
        .as_ref()
        .map(|c| {
            let provider = echo_agent_app_core::instruction_provider::InstructionProvider::load_for(
                Some(&c.root),
            );
            [
                provider.user_level.as_ref(),
                provider.project_level.as_ref(),
                provider.agents_level.as_ref(),
                provider.local_level.as_ref(),
                provider.hot_memory.as_ref(),
            ]
            .iter()
            .filter(|opt| opt.is_some())
            .count()
        })
        .unwrap_or(0);
    let project_display = project_ctx
        .as_ref()
        .map(|c| c.root.to_string_lossy().to_string());

    output.print_session_info(
        &config.mode,
        &model_name,
        project_display.as_deref(),
        instructions_count,
    );

    // Build command registry with trait-based commands
    let mut registry = crate::cli::command::CommandRegistry::new();
    crate::cli::cmd_impls::analysis::register_all(&mut registry);
    crate::cli::cmd_impls::agent_router::register_all(&mut registry);
    crate::cli::cmd_impls::coding::register_all(&mut registry);
    crate::cli::cmd_impls::diff_cmd::register_all(&mut registry);
    crate::cli::cmd_impls::developer::register_all(&mut registry);
    crate::cli::cmd_impls::git::register_all(&mut registry);
    crate::cli::cmd_impls::session::register_all(&mut registry);
    crate::cli::cmd_impls::info::register_all(&mut registry);
    crate::cli::cmd_impls::context::register_all(&mut registry);
    crate::cli::cmd_impls::advanced::register_all(&mut registry);
    crate::cli::cmd_impls::skills::register_all(&mut registry);
    crate::cli::cmd_impls::hooks::register_all(&mut registry);
    crate::cli::cmd_impls::observability::register_all(&mut registry);
    crate::cli::cmd_impls::evolution::register_all(&mut registry);
    crate::cli::cmd_impls::tasks_ext::register_all(&mut registry);
    crate::cli::cmd_impls::research::register_all(&mut registry);
    crate::cli::cmd_impls::pipelines::register_all(&mut registry);
    crate::cli::cmd_impls::pipeline::register_all(&mut registry);
    crate::cli::cmd_impls::workspace::register_all(&mut registry);
    crate::cli::cmd_impls::workflows::register_all(&mut registry);
    crate::cli::cmd_impls::extract::register_all(&mut registry);
    crate::cli::cmd_impls::plugins::register_all(&mut registry);
    crate::cli::cmd_impls::cron::register_all(&mut registry);
    crate::cli::cmd_impls::all::register_all(&mut registry);

    // Create CodingLoop for coding-mode commands (C6 fix).
    let project_root = project_ctx
        .as_ref()
        .map(|c| c.root.clone())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let coding_loop = Arc::new(tokio::sync::Mutex::new(
        crate::project::coding_loop::CodingLoop::new(&project_root),
    ));

    let interaction_mode = Arc::new(tokio::sync::RwLock::new(
        echo_agent_app_core::tasks::task_runtime::InteractionMode::Auto,
    ));
    let staged_attachments = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let cmd_handler = CommandHandler::new(agent.clone())
        .with_registry(Arc::new(registry))
        .with_coding_loop(coding_loop)
        .with_task_service_opt(config.task_service.clone())
        .with_scheduler_opt(config.scheduler_runner.clone())
        .with_plugin_runtime_opt(config.plugin_runtime.clone())
        .with_prompt_assembly(config.prompt_assembly.clone())
        .with_review_integration(config.review_integration.clone())
        .with_app_state_opt(config.app_state.clone())
        .with_conversation_id(config.conversation_id.clone())
        .with_interaction_mode(interaction_mode.clone())
        .with_staged_attachments(staged_attachments.clone());

    let editor_config = EditorConfig {
        prompt: config.prompt.clone(),
        history_file: config.history_file.clone(),
        ..Default::default()
    };
    let external_printer = hitl_session
        .external_printer
        .take()
        .ok_or_else(|| anyhow::anyhow!("REPL external printer owner is unavailable"))?;
    let live_output = hitl_session.live_output.clone();
    let mut line_editor = create_enhanced_editor(&editor_config, external_printer)?;
    let prompt = EchoPrompt::new(&config.prompt);
    let hitl_rx = &mut hitl_session.request_rx;
    let failure_rx = &mut hitl_session.failure_rx;
    let foreground_turns = app_state.session.foreground_turns.clone();
    let mut active_turn: Option<ActiveReplTurn> = None;
    let mut queued_turns = ReplTurnQueue::default();
    let mut pending_hitl = VecDeque::new();
    let mut pending_git: Option<PendingGitAction> = None;

    let repl_result: anyhow::Result<()> = 'repl: loop {
        if let Ok(error) = failure_rx.try_recv() {
            reject_pending_hitl(&mut pending_hitl, &error);
            let _ =
                cancel_and_drain_active(&foreground_turns, &mut active_turn, output.as_ref()).await;
            break Err(anyhow::anyhow!(error));
        }
        drain_pending_hitl(hitl_rx, &mut pending_hitl);
        discard_expired_hitl(&mut pending_hitl);
        if active_turn
            .as_ref()
            .is_some_and(ActiveReplTurn::is_finished)
        {
            let completed = finish_active_turn(&mut active_turn, output.as_ref()).await;
            merge_pending_git(&mut pending_git, completed);
            start_next_queued_turn(
                &agent,
                output.as_ref(),
                live_output.clone(),
                &config,
                &mut active_turn,
                &mut queued_turns,
            )
            .await;
        }

        // Reedline is the sole stdin owner. This synchronous call cannot be
        // interrupted by dropping the outer future; no background reader is
        // created or falsely treated as cancellable. Concurrent turn/HITL
        // output is routed through Reedline's external printer.
        let signal = line_editor.read_line(&prompt);
        if let Ok(error) = failure_rx.try_recv() {
            reject_pending_hitl(&mut pending_hitl, &error);
            let _ =
                cancel_and_drain_active(&foreground_turns, &mut active_turn, output.as_ref()).await;
            break Err(anyhow::anyhow!(error));
        }
        drain_pending_hitl(hitl_rx, &mut pending_hitl);
        discard_expired_hitl(&mut pending_hitl);
        if active_turn
            .as_ref()
            .is_some_and(ActiveReplTurn::is_finished)
        {
            let completed = finish_active_turn(&mut active_turn, output.as_ref()).await;
            merge_pending_git(&mut pending_git, completed);
            start_next_queued_turn(
                &agent,
                output.as_ref(),
                live_output.clone(),
                &config,
                &mut active_turn,
                &mut queued_turns,
            )
            .await;
        }

        match signal {
            Ok(Signal::Success(line)) => {
                let line = line.trim();
                match line_target(
                    line,
                    !pending_hitl.is_empty(),
                    active_turn.is_some(),
                    pending_git.is_some(),
                ) {
                    ReplLineTarget::Exit => {
                        reject_pending_hitl(&mut pending_hitl, "CLI session exited");
                        let _ = cancel_and_drain_active(
                            &foreground_turns,
                            &mut active_turn,
                            output.as_ref(),
                        )
                        .await;
                        drain_pending_hitl(hitl_rx, &mut pending_hitl);
                        reject_pending_hitl(&mut pending_hitl, "CLI session exited");
                        break Ok(());
                    }
                    ReplLineTarget::HumanLoop => {
                        let _ = resolve_front_hitl(&mut pending_hitl, line, output.as_ref());
                    }
                    ReplLineTarget::ActiveTurn => {
                        let mode = *interaction_mode.read().await;
                        let attachments = {
                            let mut staged = staged_attachments.lock().await;
                            std::mem::take(&mut *staged)
                        };
                        let queued = QueuedReplTurn {
                            message: line.to_string(),
                            interaction_mode: mode,
                            attachments,
                            task_run_resume: None,
                        };
                        if let Some(active) = active_turn.as_ref() {
                            let disposition = route_active_input(
                                &agent,
                                active,
                                queued,
                                &config,
                                &mut queued_turns,
                                output.as_ref(),
                            )
                            .await;
                            if disposition == ActiveInputDisposition::Queued {
                                let wait = match active_turn.as_mut() {
                                    Some(active) => {
                                        wait_for_queued_follow_up(
                                            active,
                                            hitl_rx,
                                            failure_rx,
                                            &mut pending_hitl,
                                            output.as_ref(),
                                            || line_editor.read_line(&prompt),
                                            async {
                                                if let Err(error) = tokio::signal::ctrl_c().await {
                                                    tracing::warn!(
                                                        %error,
                                                        "CLI Ctrl-C listener failed during queued follow-up wait"
                                                    );
                                                }
                                            },
                                        )
                                        .await
                                    }
                                    None => QueuedFollowUpWait::Settled,
                                };
                                match wait {
                                    QueuedFollowUpWait::Settled => {
                                        let completed =
                                            finish_active_turn(&mut active_turn, output.as_ref())
                                                .await;
                                        merge_pending_git(&mut pending_git, completed);
                                        start_next_queued_turn(
                                            &agent,
                                            output.as_ref(),
                                            live_output.clone(),
                                            &config,
                                            &mut active_turn,
                                            &mut queued_turns,
                                        )
                                        .await;
                                    }
                                    QueuedFollowUpWait::Exit => {
                                        reject_pending_hitl(
                                            &mut pending_hitl,
                                            "CLI session exited",
                                        );
                                        let _ = cancel_and_drain_active(
                                            &foreground_turns,
                                            &mut active_turn,
                                            output.as_ref(),
                                        )
                                        .await;
                                        drain_pending_hitl(hitl_rx, &mut pending_hitl);
                                        reject_pending_hitl(
                                            &mut pending_hitl,
                                            "CLI session exited",
                                        );
                                        break 'repl Ok(());
                                    }
                                    QueuedFollowUpWait::Interrupted => {
                                        reject_pending_hitl(
                                            &mut pending_hitl,
                                            "User interrupted the active turn",
                                        );
                                        let completed = cancel_and_drain_active(
                                            &foreground_turns,
                                            &mut active_turn,
                                            output.as_ref(),
                                        )
                                        .await;
                                        drain_pending_hitl(hitl_rx, &mut pending_hitl);
                                        reject_pending_hitl(
                                            &mut pending_hitl,
                                            "User interrupted the active turn",
                                        );
                                        merge_pending_git(&mut pending_git, completed);
                                        start_next_queued_turn(
                                            &agent,
                                            output.as_ref(),
                                            live_output.clone(),
                                            &config,
                                            &mut active_turn,
                                            &mut queued_turns,
                                        )
                                        .await;
                                    }
                                    QueuedFollowUpWait::InputClosed => {
                                        reject_pending_hitl(&mut pending_hitl, "CLI input closed");
                                        let _ = cancel_and_drain_active(
                                            &foreground_turns,
                                            &mut active_turn,
                                            output.as_ref(),
                                        )
                                        .await;
                                        drain_pending_hitl(hitl_rx, &mut pending_hitl);
                                        reject_pending_hitl(&mut pending_hitl, "CLI input closed");
                                        output.print_success("再见！");
                                        break 'repl Ok(());
                                    }
                                    QueuedFollowUpWait::ReadError(error) => {
                                        output.print_error(&format!("错误: {error}"));
                                    }
                                    QueuedFollowUpWait::SessionFailed(error) => {
                                        reject_pending_hitl(&mut pending_hitl, &error);
                                        let _ = cancel_and_drain_active(
                                            &foreground_turns,
                                            &mut active_turn,
                                            output.as_ref(),
                                        )
                                        .await;
                                        break 'repl Err(anyhow::anyhow!(error));
                                    }
                                }
                                continue 'repl;
                            }
                        } else {
                            queued_turns.enqueue(queued);
                        }
                    }
                    ReplLineTarget::GitAction => {
                        if let Some(action) = pending_git.take() {
                            handle_git_action(line, action.changes, output.as_ref());
                        }
                    }
                    ReplLineTarget::Idle => {
                        if line.is_empty() {
                            continue;
                        }
                        match cmd_handler.handle(line).await {
                            CommandResult::Continue => {}
                            CommandResult::Exit => break Ok(()),
                            CommandResult::Chat(message) => {
                                let mode = *interaction_mode.read().await;
                                let attachments = {
                                    let mut staged = staged_attachments.lock().await;
                                    std::mem::take(&mut *staged)
                                };
                                let input = QueuedReplTurn {
                                    message,
                                    attachments,
                                    interaction_mode: mode,
                                    task_run_resume: None,
                                };
                                // Every idle chat enters the same queue before
                                // admission. If an older head was retained by
                                // Busy/AdmissionSuspended, it always starts first.
                                enqueue_idle_input(&mut queued_turns, input);
                                start_next_queued_turn(
                                    &agent,
                                    output.as_ref(),
                                    live_output.clone(),
                                    &config,
                                    &mut active_turn,
                                    &mut queued_turns,
                                )
                                .await;
                            }
                            CommandResult::ResumeTaskRun {
                                message,
                                run_id,
                                root_message_id,
                            } => {
                                let input = QueuedReplTurn {
                                    message,
                                    attachments: Vec::new(),
                                    interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode::Task,
                                    task_run_resume: Some(ExplicitTaskRunResume {
                                        run_id,
                                        root_message_id,
                                    }),
                                };
                                enqueue_idle_input(&mut queued_turns, input);
                                start_next_queued_turn(
                                    &agent,
                                    output.as_ref(),
                                    live_output.clone(),
                                    &config,
                                    &mut active_turn,
                                    &mut queued_turns,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
            Ok(Signal::CtrlC) => {
                reject_pending_hitl(&mut pending_hitl, "User interrupted the active turn");
                if active_turn.is_some() {
                    let completed = cancel_and_drain_active(
                        &foreground_turns,
                        &mut active_turn,
                        output.as_ref(),
                    )
                    .await;
                    drain_pending_hitl(hitl_rx, &mut pending_hitl);
                    reject_pending_hitl(&mut pending_hitl, "User interrupted the active turn");
                    merge_pending_git(&mut pending_git, completed);
                    start_next_queued_turn(
                        &agent,
                        output.as_ref(),
                        live_output.clone(),
                        &config,
                        &mut active_turn,
                        &mut queued_turns,
                    )
                    .await;
                } else {
                    output.print_info("（输入 /exit 退出）");
                }
            }
            Ok(Signal::CtrlD) => {
                reject_pending_hitl(&mut pending_hitl, "CLI input closed");
                let _ =
                    cancel_and_drain_active(&foreground_turns, &mut active_turn, output.as_ref())
                        .await;
                drain_pending_hitl(hitl_rx, &mut pending_hitl);
                reject_pending_hitl(&mut pending_hitl, "CLI input closed");
                output.print_success("再见！");
                break Ok(());
            }
            Err(err) => {
                output.print_error(&format!("错误: {}", err));
            }
        }
    };
    let mut abandoned_attachments = {
        let mut staged = staged_attachments.lock().await;
        std::mem::take(&mut *staged)
    };
    abandoned_attachments.extend(queued_turns.discard_attachments());
    let cleanup =
        echo_agent_app_core::attachments::discard_staged_attachment_refs(&abandoned_attachments);
    match (repl_result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(cleanup)) => Err(anyhow::anyhow!(
            "failed to clean abandoned CLI attachment staging: {cleanup}"
        )),
        (Err(error), Err(cleanup)) => Err(anyhow::anyhow!(
            "CLI session failed: {error}; attachment staging cleanup failed: {cleanup}"
        )),
    }
}

/// Run auto-memory extraction when the session ends.
///
/// Non-blocking: errors are silently ignored to avoid disrupting exit flow.
pub(crate) async fn run_auto_memory_on_exit(
    agent: &AgentHandle,
    review_integration: &Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
) {
    use echo_agent_app_core::auto_memory::{
        AutoMemoryConfig, extract_observations, queue_observations,
    };

    // Check if auto-memory is enabled (shared with /auto-memory command)
    // Use the global flag from the cmd_impls module
    let enabled =
        crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED.load(std::sync::atomic::Ordering::Relaxed);
    if !enabled {
        return;
    }

    let Some(integration) = review_integration.as_ref() else {
        println!("  Auto-memory: Review integration is not configured.");
        return;
    };
    let evidence_lease = match integration.lease_generation() {
        Ok(lease) => lease,
        Err(error) => {
            println!("  Auto-memory: workspace is switching; candidates were not queued ({error})");
            return;
        }
    };

    // Extract messages from the agent context
    let messages: Vec<(String, String)> = agent
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                ctx.messages()
                    .iter()
                    .map(|m| {
                        (
                            m.role.as_str().to_string(),
                            m.content.as_text().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
        })
        .await;

    // Need a minimum number of messages to extract meaningful observations
    if messages.len() < 2 {
        return;
    }

    let config = AutoMemoryConfig::default();
    let observations = extract_observations(&messages, &config);

    if observations.is_empty() {
        return;
    }

    let store = evidence_lease.evidence_store();
    match queue_observations(&store, &observations, &messages) {
        Ok(candidates) => println!(
            "  Auto-memory: queued {} observation candidate(s) for review.",
            candidates.len()
        ),
        Err(error) => println!("  Auto-memory: failed to queue candidates ({error})"),
    }
}

/// Run memory review when the session ends.
///
/// Performs analysis-only staleness scoring and conflict detection on typed
/// memories, then queues actionable proposals in the Review Inbox. Non-blocking:
/// errors are reported without disrupting exit flow.
///
pub(crate) async fn run_memory_review_on_exit(
    shared_ri: &Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
) {
    let Some(ri) = shared_ri else {
        tracing::debug!("session-end memory review skipped: ReviewIntegration is not configured");
        return;
    };
    if let Some(review_result) = ri.on_session_end().await {
        match review_result {
            Ok(report) => {
                let count = report.total_scanned;
                if count > 0 {
                    println!(
                        "  📋 Memory review: {} scanned, {} stale, {} conflicts, {} proposals queued",
                        count,
                        report.stale_count,
                        report.conflict_groups,
                        report.conflict_proposals.len()
                    );
                }
            }
            Err(e) => {
                eprintln!("  ⚠ Memory review failed: {e}");
            }
        }
    }
}

fn drain_pending_hitl(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<
        echo_agent_app_core::hitl::PendingReplHumanLoopRequest,
    >,
    pending: &mut VecDeque<echo_agent_app_core::hitl::PendingReplHumanLoopRequest>,
) {
    while let Ok(request) = receiver.try_recv() {
        pending.push_back(request);
    }
}

fn discard_expired_hitl(
    pending: &mut VecDeque<echo_agent_app_core::hitl::PendingReplHumanLoopRequest>,
) {
    pending.retain(|request| !request.is_expired());
}

fn resolve_front_hitl(
    pending: &mut VecDeque<echo_agent_app_core::hitl::PendingReplHumanLoopRequest>,
    input: &str,
    output: &OutputRenderer,
) -> bool {
    discard_expired_hitl(pending);
    let Some(request) = pending.pop_front() else {
        return false;
    };
    let request_id = request.request_id().to_string();
    match request.resolve(input) {
        Ok(()) => output.print_info(&format!("Human input submitted for request {request_id}")),
        Err(error) => output.print_warning(&format!(
            "Human input request {request_id} expired: {error}"
        )),
    }
    true
}

fn reject_pending_hitl(
    pending: &mut VecDeque<echo_agent_app_core::hitl::PendingReplHumanLoopRequest>,
    reason: &str,
) {
    while let Some(request) = pending.pop_front() {
        if !request.is_expired() {
            let _ = request.reject(reason.to_string());
        }
    }
}

fn merge_pending_git(target: &mut Option<PendingGitAction>, incoming: Option<PendingGitAction>) {
    let Some(incoming) = incoming else {
        return;
    };
    if let Some(current) = target.as_mut() {
        current.changes = current.changes.saturating_add(incoming.changes);
    } else {
        *target = Some(incoming);
    }
}

async fn finish_active_turn(
    active: &mut Option<ActiveReplTurn>,
    output: &OutputRenderer,
) -> Option<PendingGitAction> {
    let result = {
        let active = active.as_mut()?;
        let task = active.task.as_mut()?;
        task.await
    };
    let mut completed = active.take()?;
    completed.cancel_on_drop = false;
    if let Some(task) = completed.task.take() {
        drop(task);
    }
    match result {
        Ok(changes) if changes > 0 => Some(PendingGitAction { changes }),
        Ok(_) => None,
        Err(error) => {
            output.print_error(&format!("CLI turn renderer failed: {error}"));
            None
        }
    }
}

async fn cancel_and_drain_active(
    control: &echo_agent_app_core::foreground_turn::ForegroundTurnControl,
    active: &mut Option<ActiveReplTurn>,
    output: &OutputRenderer,
) -> Option<PendingGitAction> {
    let identity = active.as_ref()?;
    let waiter = match control.request_cancel_scoped(
        &identity.workspace_id,
        echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
        &identity.conversation_id,
        &identity.turn_id,
    ) {
        Ok(waiter) => Some(waiter),
        Err(echo_agent_app_core::foreground_turn::ForegroundTurnError::NoActiveTurn { .. }) => None,
        Err(error) => {
            output.print_warning(&format!("CLI turn cancellation was not accepted: {error}"));
            None
        }
    };
    if let Some(waiter) = waiter {
        match waiter.wait().await {
            Ok(settlement) => output.print_info(&format!(
                "Turn {} settled as {:?}",
                settlement.turn_id, settlement.outcome
            )),
            Err(error) => {
                output.print_warning(&format!("CLI turn settlement was unavailable: {error}"));
            }
        }
    }
    finish_active_turn(active, output).await
}

async fn start_next_queued_turn(
    agent: &AgentHandle,
    output: &OutputRenderer,
    live_output: ReplExternalOutput,
    config: &ReplConfig,
    active: &mut Option<ActiveReplTurn>,
    queued: &mut ReplTurnQueue,
) {
    let Some(next) = queued.front_for_idle(active.is_some()) else {
        return;
    };
    match prepare_repl_turn_start(agent, next, config).await {
        Ok(prepared) => {
            let Some(next) = queued.consume_front() else {
                prepared
                    .lease
                    .settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "repl_queue",
                            "queued turn disappeared after foreground admission",
                        ),
                    ));
                output.print_error("Queued turn disappeared after foreground admission");
                return;
            };
            *active = Some(spawn_prepared_repl_turn(
                agent,
                next,
                output,
                live_output,
                config,
                prepared,
            ));
        }
        Err(error) => match queued.settle_start_failure(&error) {
            QueuedStartFailureDisposition::Retained => output.print_info(&format!(
                "Queued follow-up remains pending: {}",
                error.message()
            )),
            QueuedStartFailureDisposition::Consumed => output.print_error(&format!(
                "Queued follow-up failed permanently and was consumed: {}",
                error.message()
            )),
        },
    }
}

async fn route_active_input(
    agent: &AgentHandle,
    active: &ActiveReplTurn,
    input: QueuedReplTurn,
    config: &ReplConfig,
    queued: &mut ReplTurnQueue,
    output: &OutputRenderer,
) -> ActiveInputDisposition {
    let workspace_root = match config.app_state.as_ref() {
        Some(state) => state
            .current_workspace()
            .await
            .map(|workspace| workspace.root),
        None => None,
    };
    let spill_dir =
        echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(workspace_root.as_deref());
    let prepared = echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::prepared_turn::UserTurnInput {
            text: &input.message,
            attachments: &input.attachments,
            spill_dir: &spill_dir,
            conversation_id: Some(&active.conversation_id),
            turn_id: Some(&active.turn_id),
        },
    );

    match prepared {
        Ok(prepared) => {
            // Preparation may replace and remove a staged paste source. Queue
            // the durable instruction/artifact projection so a steer race can
            // never leave the FIFO pointing at a deleted temporary file.
            let fallback = queued_turn_from_prepared(&input, &prepared);
            match prepared.to_message() {
                Ok(message) => match settle_steer_attempt(
                    agent.steer_input(Some(&active.turn_id), message).await,
                    fallback,
                    queued,
                ) {
                    Ok(turn_id) => {
                        output.print_info(&format!("Guidance injected into turn {turn_id}"));
                        ActiveInputDisposition::Steered
                    }
                    Err(
                        echo_agent::agent::TurnSteerError::NoActiveTurn
                        | echo_agent::agent::TurnSteerError::NotSteerable { .. }
                        | echo_agent::agent::TurnSteerError::TurnMismatch { .. },
                    ) => {
                        output.print_info(&format!(
                            "Current stage is not steerable; queued {} follow-up(s)",
                            queued.len()
                        ));
                        ActiveInputDisposition::Queued
                    }
                    Err(error) => {
                        output.print_warning(&format!(
                            "Steer failed ({error}); queued {} follow-up(s)",
                            queued.len()
                        ));
                        ActiveInputDisposition::Queued
                    }
                },
                Err(error) => {
                    queued.enqueue(fallback);
                    output.print_warning(&format!(
                        "Could not encode steer input ({error}); queued {} follow-up(s)",
                        queued.len()
                    ));
                    ActiveInputDisposition::Queued
                }
            }
        }
        Err(error) => {
            queued.enqueue(input);
            output.print_warning(&format!(
                "Could not prepare steer input ({error}); queued {} follow-up(s)",
                queued.len()
            ));
            ActiveInputDisposition::Queued
        }
    }
}

fn queued_turn_from_prepared(
    input: &QueuedReplTurn,
    prepared: &echo_agent_app_core::prepared_turn::PreparedUserTurn,
) -> QueuedReplTurn {
    QueuedReplTurn {
        message: prepared.instruction.clone(),
        interaction_mode: input.interaction_mode,
        attachments: prepared.inline_attachment_refs(),
        task_run_resume: input.task_run_resume.clone(),
    }
}

fn handle_git_action(input: &str, changes: usize, output: &OutputRenderer) {
    let choice = input
        .trim()
        .chars()
        .next()
        .map(|value| value.to_ascii_lowercase());
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            output.print_warning(&format!("Cannot resolve git working directory: {error}"));
            return;
        }
    };
    let result = match choice {
        Some('c') => crate::cli::git_ops::interactive_commit(&cwd, changes),
        Some('s') => crate::cli::git_ops::interactive_stage(&cwd),
        _ => return,
    };
    if let Err(error) = result {
        output.print_error(&format!("Git action failed: {error}"));
    }
}

/// Acquire the authoritative foreground lease and prepare an immutable turn.
async fn prepare_repl_turn_start(
    agent: &AgentHandle,
    input: &QueuedReplTurn,
    config: &ReplConfig,
) -> Result<PreparedReplTurnStart, ReplTurnStartError> {
    let app_state = config.app_state.as_ref().ok_or_else(|| {
        ReplTurnStartError::Permanent("CLI foreground turn control is unavailable".to_string())
    })?;
    let turn_id = uuid::Uuid::new_v4().to_string();
    let conversation_id = agent
        .read(|value| value.conversation_id().map(str::to_string))
        .await
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| config.conversation_id.clone());
    let control = app_state.session.foreground_turns.clone();
    let (scoped_runtime, lease) = app_state
        .begin_scoped_chat_turn_owned(
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
            &conversation_id,
            turn_id.clone(),
        )
        .await
        .map_err(ReplTurnStartError::from_scoped_admission)?;
    let pool_execution = match scoped_runtime.agent_for(&conversation_id).await {
        Ok(execution) => execution,
        Err(error) => {
            let detail = format!("CLI AgentPool admission failed: {error}");
            lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message("agent_pool", detail.clone()),
            ));
            return Err(ReplTurnStartError::Permanent(detail));
        }
    };
    let workspace_root = match config.app_state.as_ref() {
        Some(state) => state
            .current_workspace()
            .await
            .map(|workspace| workspace.root),
        None => config
            .project
            .as_deref()
            .map(std::path::Path::new)
            .and_then(|path| path.canonicalize().ok()),
    };
    let spill_dir =
        echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(workspace_root.as_deref());
    let runtime_authored = input.task_run_resume.is_some();
    let turn = match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::prepared_turn::UserTurnInput {
            text: &input.message,
            attachments: &input.attachments,
            spill_dir: &spill_dir,
            conversation_id: Some(&conversation_id),
            turn_id: Some(&turn_id),
        },
    ) {
        Ok(turn) if runtime_authored => turn.runtime_authored(),
        Ok(turn) => turn,
        Err(error) => {
            let detail = format!("Failed to prepare user turn: {error}");
            lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message("prepared_turn", detail.clone()),
            ));
            return Err(ReplTurnStartError::Permanent(detail));
        }
    };
    Ok(PreparedReplTurnStart {
        scoped_runtime,
        pool_execution,
        conversation_id,
        turn_id,
        turn,
        control,
        lease,
    })
}

fn spawn_prepared_repl_turn(
    _agent: &AgentHandle,
    input: QueuedReplTurn,
    output: &OutputRenderer,
    live_output: ReplExternalOutput,
    config: &ReplConfig,
    prepared: PreparedReplTurnStart,
) -> ActiveReplTurn {
    let PreparedReplTurnStart {
        scoped_runtime,
        pool_execution,
        conversation_id,
        turn_id,
        turn,
        control,
        lease,
    } = prepared;
    let cancel = lease.cancellation_token();
    let renderer = Arc::new(ReplChatSink::new(live_output.clone(), output.config()));
    let render_sink: Arc<dyn echo_agent_app_core::chat_driver::ChatSink> = renderer.clone();
    let sink = config
        .app_state
        .as_ref()
        .map_or(render_sink.clone(), |state| {
            echo_agent_app_core::chat_event_log::bind_surface_chat_sink(
                echo_agent_app_core::chat_event_log::ChatSurface::Cli,
                render_sink,
                state.storage.chat_events.clone(),
                state.storage.tool_executions.clone(),
                scoped_runtime.execution_scope().workspace_id().to_string(),
                Some(conversation_id.clone()),
                turn_id.clone(),
            )
        });
    let _ = live_output.bind_turn_cancel(cancel.clone());
    let resources = Arc::new(echo_agent_app_core::chat_resources::ChatResources {
        execution_scope: scoped_runtime.execution_scope().clone(),
        pool: scoped_runtime.pool(),
        store: scoped_runtime.task_runtime(),
        sink,
        webhook_emitter: config.webhook_emitter.clone(),
        conv_id: Some(conversation_id.clone()),
        root_message_id: turn_id.clone(),
        attachments: turn.inline_attachment_refs(),
        cancel,
        interaction_mode: input.interaction_mode,
        review_integration: scoped_runtime.review_integration(),
        layer_manager: None,
        memory_generation: None,
        human_loop_provider: config.app_state.as_ref().map(|state| {
            state.connection.hitl_dispatcher.clone()
                as Arc<dyn echo_agent::human_loop::HumanLoopProvider>
        }),
    });
    let agent_owned = pool_execution.agent();
    let workspace_id = scoped_runtime.execution_scope().workspace_id().to_string();
    let bound_turn_id = turn_id.clone();
    let (completion_tx, completion) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _pool_execution = pool_execution;
        let _ = live_output.print_user_message(&input.message);
        let _ = live_output.emit("Connecting to model...");
        let result = match input.task_run_resume {
            Some(resume) => {
                echo_agent_app_core::foreground_turn::drive_foreground_chat_turn(
                    lease,
                    &agent_owned,
                    &turn,
                    resources,
                    echo_agent_app_core::tasks::task_runtime::RunTurnBinding {
                        run_id: Some(resume.run_id),
                        turn_id: bound_turn_id,
                        root_message_id: resume.root_message_id,
                        origin: echo_agent_app_core::tasks::task_runtime::RunTurnOrigin::Resume,
                        transcript_visibility:
                            echo_agent_app_core::tasks::task_runtime::TurnVisibility::Visible,
                    },
                )
                .await
            }
            None => {
                echo_agent_app_core::foreground_turn::drive_foreground_chat(
                    lease,
                    &agent_owned,
                    &turn,
                    resources,
                )
                .await
            }
        };
        let changes = renderer.finish(&result);
        live_output.clear_turn_cancel();
        let _ = completion_tx.send(());
        changes
    });

    ActiveReplTurn {
        workspace_id,
        conversation_id,
        turn_id,
        control,
        task: Some(task),
        completion: Some(completion),
        cancel_on_drop: true,
    }
}
/// 自定义提示符
struct EchoPrompt {
    prompt: String,
}

impl EchoPrompt {
    fn new(prompt: &str) -> Self {
        Self {
            prompt: prompt.to_string(),
        }
    }
}

impl Prompt for EchoPrompt {
    fn render_prompt_left(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Owned(format!("{} > ", Color::Green.bold().paint(&self.prompt)))
    }

    fn render_prompt_right(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("")
    }

    fn render_prompt_indicator(
        &self,
        _prompt_mode: reedline::PromptEditMode,
    ) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("... ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: reedline::PromptHistorySearch,
    ) -> std::borrow::Cow<'_, str> {
        let prefix = match history_search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };

        std::borrow::Cow::Owned(format!(
            "({}reverse-search: {}) ",
            prefix, history_search.term
        ))
    }
}

// ── 单元测试 ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};

    #[test]
    fn test_repl_config_default() {
        let config = ReplConfig::default();
        assert_eq!(config.prompt, "echo");
    }

    #[test]
    fn test_echo_prompt() {
        let prompt = EchoPrompt::new("test");
        let left = prompt.render_prompt_left();
        assert!(left.contains("test"));
    }

    #[tokio::test]
    async fn repl_hitl_session_registers_before_bootstrap_and_rejects_on_shutdown()
    -> Result<(), String> {
        let dispatcher = Arc::new(echo_agent_app_core::hitl::HitlDispatcher::new());
        let mut session = ReplHumanLoopSession::register(Arc::clone(&dispatcher)).await;
        if dispatcher.provider_count().await != 1 {
            return Err("REPL provider was not registered by the session owner".to_string());
        }
        let request_dispatcher = Arc::clone(&dispatcher);
        let response = tokio::spawn(async move {
            request_dispatcher
                .request(HumanLoopRequest::input("bootstrap input"))
                .await
        });
        let pending = session
            .request_rx
            .recv()
            .await
            .ok_or_else(|| "bootstrap HITL request was not queued".to_string())?;

        session
            .shutdown("CLI bootstrap failed")
            .await
            .map_err(|error| error.to_string())?;
        if dispatcher.provider_count().await != 0 || !pending.is_expired() {
            return Err(
                "REPL session shutdown left registration or exact request live".to_string(),
            );
        }
        let response = response
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        if !matches!(
            response,
            HumanLoopResponse::Rejected { reason: Some(reason) }
                if reason == "CLI bootstrap failed"
        ) {
            return Err("bootstrap shutdown did not reject the exact requester".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn aborted_repl_owner_unregisters_exact_provider_before_reregister() -> Result<(), String>
    {
        let dispatcher = Arc::new(echo_agent_app_core::hitl::HitlDispatcher::new());
        let first = ReplHumanLoopSession::register(Arc::clone(&dispatcher)).await;
        let mut replacement = ReplHumanLoopSession::register(Arc::clone(&dispatcher)).await;
        if dispatcher.provider_count().await != 2 {
            return Err("overlapping REPL registrations were not both observable".to_string());
        }
        let aborted = tokio::spawn(async move {
            let _session = first;
            std::future::pending::<()>().await;
        });
        aborted.abort();
        let _aborted_result = aborted.await;
        if dispatcher.provider_count().await != 1 {
            return Err(
                "aborted REPL owner removed the replacement or left its own provider registered"
                    .to_string(),
            );
        }
        let request_dispatcher = Arc::clone(&dispatcher);
        let response = tokio::spawn(async move {
            request_dispatcher
                .request(HumanLoopRequest::input("replacement input"))
                .await
        });
        let pending = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            replacement.request_rx.recv(),
        )
        .await
        .map_err(|_| "replacement provider did not receive the request".to_string())?
        .ok_or_else(|| "replacement request channel closed".to_string())?;
        pending
            .resolve("replacement answer")
            .map_err(|error| error.to_string())?;
        let response = response
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        if !matches!(response, HumanLoopResponse::Text(text) if text == "replacement answer") {
            return Err("stale provider defeated the replacement registration".to_string());
        }
        replacement
            .shutdown("replacement complete")
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn queued_turn(message: &str) -> QueuedReplTurn {
        QueuedReplTurn {
            message: message.to_string(),
            interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode::Auto,
            attachments: Vec::new(),
            task_run_resume: None,
        }
    }

    #[test]
    fn broker_priority_and_failed_steer_queue_start_only_once() {
        assert_eq!(line_target("/exit", true, true, true), ReplLineTarget::Exit);
        assert_eq!(
            line_target("approval", true, true, true),
            ReplLineTarget::HumanLoop
        );
        assert_eq!(
            line_target("guidance", false, true, true),
            ReplLineTarget::ActiveTurn
        );

        // Inject a real framework steer failure through the production helper.
        let mut queue = ReplTurnQueue::default();
        let steer = settle_steer_attempt(
            Err(echo_agent::agent::TurnSteerError::NotSteerable {
                turn_id: "active".to_string(),
            }),
            queued_turn("first follow-up"),
            &mut queue,
        );
        assert!(matches!(
            steer,
            Err(echo_agent::agent::TurnSteerError::NotSteerable { .. })
        ));
        queue.enqueue(queued_turn("second follow-up"));
        let mut has_active_turn = false;
        let mut starts = 0_usize;
        if queue.front_for_idle(has_active_turn).is_some() {
            let _ = queue.consume_front();
            has_active_turn = true;
            starts = starts.saturating_add(1);
        }
        if queue.front_for_idle(has_active_turn).is_some() {
            let _ = queue.consume_front();
            starts = starts.saturating_add(1);
        }
        assert_eq!(starts, 1);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn queued_admission_retries_preserve_fifo_and_permanent_failure_consumes_front()
    -> Result<(), String> {
        use echo_agent_app_core::foreground_turn::{
            ForegroundTurnControl, ForegroundTurnError, ForegroundTurnSurface,
        };

        let mut queue = ReplTurnQueue::default();
        queue.enqueue(queued_turn("first"));
        queue.enqueue(queued_turn("second"));
        let control = ForegroundTurnControl::default();
        let _active = control
            .begin(ForegroundTurnSurface::Cli, "conversation", "active")
            .map_err(|error| error.to_string())?;
        let busy = control
            .begin(ForegroundTurnSurface::Cli, "conversation", "next")
            .err()
            .map(ReplTurnStartError::from_admission)
            .ok_or_else(|| "busy foreground admission was accepted".to_string())?;
        assert_eq!(
            queue.settle_start_failure(&busy),
            QueuedStartFailureDisposition::Retained
        );
        assert_eq!(queue.len(), 2);
        assert_eq!(
            queue
                .front_for_idle(false)
                .map(|turn| turn.message.as_str()),
            Some("first")
        );

        let suspended = ReplTurnStartError::from_admission(ForegroundTurnError::AdmissionSuspended);
        assert_eq!(
            queue.settle_start_failure(&suspended),
            QueuedStartFailureDisposition::Retained
        );
        assert_eq!(queue.len(), 2);

        let permanent = ReplTurnStartError::Permanent("invalid persisted attachment".to_string());
        assert_eq!(
            queue.settle_start_failure(&permanent),
            QueuedStartFailureDisposition::Consumed
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(
            queue
                .front_for_idle(false)
                .map(|turn| turn.message.as_str()),
            Some("second")
        );
        Ok(())
    }

    #[test]
    fn production_idle_input_cannot_bypass_a_retained_fifo_head() {
        let mut queue = ReplTurnQueue::default();
        queue.enqueue(queued_turn("retained-first"));
        let suspended = ReplTurnStartError::from_admission(
            echo_agent_app_core::foreground_turn::ForegroundTurnError::AdmissionSuspended,
        );
        assert_eq!(
            queue.settle_start_failure(&suspended),
            QueuedStartFailureDisposition::Retained
        );

        // This is the same helper used by the production Idle -> Chat branch.
        enqueue_idle_input(&mut queue, queued_turn("new-idle-input"));
        assert_eq!(
            queue.consume_front().map(|turn| turn.message),
            Some("retained-first".to_string())
        );
        assert_eq!(
            queue.consume_front().map(|turn| turn.message),
            Some("new-idle-input".to_string())
        );
    }

    #[test]
    fn prepared_steer_fallback_keeps_spilled_paste_durable() -> Result<(), String> {
        let root = std::env::temp_dir().join(format!("eko-repl-steer-{}", uuid::Uuid::new_v4()));
        let staging = root
            .join(".eko")
            .join("uploads")
            .join(format!("{}_paste.txt", uuid::Uuid::new_v4()));
        let artifacts = root.join("artifacts");
        let parent = staging
            .parent()
            .ok_or_else(|| "paste staging path has no parent".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::write(&staging, "粘贴内容🙂").map_err(|error| error.to_string())?;
        let input = QueuedReplTurn {
            message: "use the attached paste".to_string(),
            interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode::Auto,
            attachments: vec![echo_agent_app_core::attachments::AttachmentRef {
                path: staging.clone(),
                name: "paste.txt".to_string(),
                mime_type: "text/plain".to_string(),
                source: echo_agent_app_core::types::AttachmentSource::Paste,
            }],
            task_run_resume: None,
        };
        let prepared = echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
            echo_agent_app_core::prepared_turn::UserTurnInput {
                text: &input.message,
                attachments: &input.attachments,
                spill_dir: &artifacts,
                conversation_id: Some("conversation"),
                turn_id: Some("turn"),
            },
        )
        .map_err(|error| error.to_string())?;
        let artifact = prepared
            .resources
            .first()
            .ok_or_else(|| "prepared paste did not produce an artifact".to_string())?;
        if staging.exists()
            || !artifact.path.exists()
            || !prepared
                .instruction
                .contains(&artifact.path.display().to_string())
        {
            return Err("prepared paste did not replace its staging path durably".to_string());
        }

        let fallback = queued_turn_from_prepared(&input, &prepared);
        if !fallback.attachments.is_empty()
            || !fallback
                .message
                .contains(&artifact.path.display().to_string())
            || !artifact.path.exists()
        {
            return Err("steer fallback lost its durable paste artifact".to_string());
        }
        std::fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn queued_follow_up_broker_handles_hitl_then_wakes_on_settlement_once()
    -> Result<(), String> {
        let (provider, mut hitl_rx, mut failure_rx) =
            echo_agent_app_core::hitl::ReplHumanLoopProvider::channel(Arc::new(
                |_prompt: String| Ok(()),
            ));
        let provider = Arc::new(provider);
        let mut request = HumanLoopRequest::input("answer before settling");
        request.request_id = Some("queued-follow-up-hitl".to_string());
        let response_provider = Arc::clone(&provider);
        let response_task = tokio::spawn(async move { response_provider.request(request).await });

        let control = echo_agent_app_core::foreground_turn::ForegroundTurnControl::default();
        let lease = control
            .begin(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                "queued-follow-up-conversation",
                "queued-follow-up-turn",
            )
            .map_err(|error| error.to_string())?;
        let response_was_exact = Arc::new(AtomicBool::new(false));
        let response_flag = Arc::clone(&response_was_exact);
        let (completion_tx, completion) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let exact = matches!(
                response_task.await,
                Ok(Ok(HumanLoopResponse::Text(value))) if value == "broker answer"
            );
            response_flag.store(exact, Ordering::Release);
            lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Completed);
            let _ = completion_tx.send(());
            0
        });
        let mut active = Some(ActiveReplTurn {
            workspace_id: "global".to_string(),
            conversation_id: "queued-follow-up-conversation".to_string(),
            turn_id: "queued-follow-up-turn".to_string(),
            control: control.clone(),
            task: Some(task),
            completion: Some(completion),
            cancel_on_drop: true,
        });

        let mut queue = ReplTurnQueue::default();
        let steer = settle_steer_attempt(
            Err(echo_agent::agent::TurnSteerError::NotSteerable {
                turn_id: "queued-follow-up-turn".to_string(),
            }),
            queued_turn("start after settlement"),
            &mut queue,
        );
        if steer.is_ok() || queue.len() != 1 {
            return Err("failed steer did not enqueue exactly one follow-up".to_string());
        }

        let mut pending_hitl = VecDeque::new();
        let mut injected_signals = VecDeque::from([Signal::Success("broker answer".to_string())]);
        let read_count = Arc::new(AtomicUsize::new(0));
        let reads = Arc::clone(&read_count);
        let test_output = OutputRenderer::default();
        let wait = match active.as_mut() {
            Some(active) => {
                wait_for_queued_follow_up(
                    active,
                    &mut hitl_rx,
                    &mut failure_rx,
                    &mut pending_hitl,
                    &test_output,
                    move || {
                        reads.fetch_add(1, Ordering::Relaxed);
                        Ok(injected_signals.pop_front().unwrap_or(Signal::CtrlD))
                    },
                    std::future::pending::<()>(),
                )
                .await
            }
            None => return Err("test foreground turn was not active".to_string()),
        };
        if wait != QueuedFollowUpWait::Settled {
            return Err(format!("unexpected queued follow-up wait result: {wait:?}"));
        }
        let _ = finish_active_turn(&mut active, &test_output).await;
        if active.is_some()
            || !response_was_exact.load(Ordering::Acquire)
            || read_count.load(Ordering::Relaxed) != 1
        {
            return Err("HITL was not resolved exactly before typed settlement".to_string());
        }
        if control
            .snapshot(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                "queued-follow-up-conversation",
            )
            .is_some()
        {
            return Err("foreground registry remained active after completion wake".to_string());
        }

        let mut has_active_turn = active.is_some();
        let mut starts = 0_usize;
        if queue.front_for_idle(has_active_turn).is_some() {
            let _ = queue.consume_front();
            has_active_turn = true;
            starts = starts.saturating_add(1);
        }
        if queue.front_for_idle(has_active_turn).is_some() {
            let _ = queue.consume_front();
            starts = starts.saturating_add(1);
        }
        if starts != 1 {
            return Err(format!("queued next turn started {starts} times"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn queued_follow_up_wait_maps_injected_interrupt_to_exact_settlement()
    -> Result<(), String> {
        let (_provider, mut hitl_rx, mut failure_rx) =
            echo_agent_app_core::hitl::ReplHumanLoopProvider::channel(Arc::new(
                |_prompt: String| Ok(()),
            ));
        let control = echo_agent_app_core::foreground_turn::ForegroundTurnControl::default();
        let lease = control
            .begin(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                "interrupt-conversation",
                "interrupt-turn",
            )
            .map_err(|error| error.to_string())?;
        let cancel = lease.cancellation_token();
        let (completion_tx, completion) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            cancel.cancelled().await;
            lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Cancelled);
            let _ = completion_tx.send(());
            0
        });
        let mut active = Some(ActiveReplTurn {
            workspace_id: "global".to_string(),
            conversation_id: "interrupt-conversation".to_string(),
            turn_id: "interrupt-turn".to_string(),
            control: control.clone(),
            task: Some(task),
            completion: Some(completion),
            cancel_on_drop: true,
        });
        let (interrupt_tx, interrupt_rx) = tokio::sync::oneshot::channel();
        if interrupt_tx.send(()).is_err() {
            return Err("injected Ctrl-C signal receiver closed early".to_string());
        }
        let read_count = Arc::new(AtomicUsize::new(0));
        let reads = Arc::clone(&read_count);
        let mut pending_hitl = VecDeque::new();
        let test_output = OutputRenderer::default();
        let wait = match active.as_mut() {
            Some(active) => {
                wait_for_queued_follow_up(
                    active,
                    &mut hitl_rx,
                    &mut failure_rx,
                    &mut pending_hitl,
                    &test_output,
                    move || {
                        reads.fetch_add(1, Ordering::Relaxed);
                        Ok(Signal::CtrlD)
                    },
                    async move {
                        let _ = interrupt_rx.await;
                    },
                )
                .await
            }
            None => return Err("interrupt test foreground turn was not active".to_string()),
        };
        if wait != QueuedFollowUpWait::Interrupted || read_count.load(Ordering::Relaxed) != 0 {
            return Err("parked wait did not prioritize injected Ctrl-C without stdin".to_string());
        }
        let _ = cancel_and_drain_active(&control, &mut active, &test_output).await;
        if active.is_some()
            || control
                .snapshot(
                    echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                    "interrupt-conversation",
                )
                .is_some()
        {
            return Err(
                "injected Ctrl-C did not exact-cancel and settle the foreground turn".into(),
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn queued_follow_up_wait_fails_without_stdin_when_prompt_sink_fails() -> Result<(), String>
    {
        let (provider, mut hitl_rx, mut failure_rx) =
            echo_agent_app_core::hitl::ReplHumanLoopProvider::channel(Arc::new(
                |_prompt: String| Err("external printer closed".to_string()),
            ));
        let response = provider
            .request(HumanLoopRequest::input("cannot display this prompt"))
            .await
            .map_err(|error| error.to_string())?;
        if !matches!(response, HumanLoopResponse::Rejected { .. }) {
            return Err("prompt sink failure did not reject its exact request".to_string());
        }

        let control = echo_agent_app_core::foreground_turn::ForegroundTurnControl::default();
        let lease = control
            .begin(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                "sink-failure-conversation",
                "sink-failure-turn",
            )
            .map_err(|error| error.to_string())?;
        let cancel = lease.cancellation_token();
        let (completion_tx, completion) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            cancel.cancelled().await;
            lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Cancelled);
            let _ = completion_tx.send(());
            0
        });
        let mut active = Some(ActiveReplTurn {
            workspace_id: "global".to_string(),
            conversation_id: "sink-failure-conversation".to_string(),
            turn_id: "sink-failure-turn".to_string(),
            control: control.clone(),
            task: Some(task),
            completion: Some(completion),
            cancel_on_drop: true,
        });
        let read_count = Arc::new(AtomicUsize::new(0));
        let reads = Arc::clone(&read_count);
        let mut pending_hitl = VecDeque::new();
        let output = OutputRenderer::default();
        let wait = match active.as_mut() {
            Some(active) => {
                wait_for_queued_follow_up(
                    active,
                    &mut hitl_rx,
                    &mut failure_rx,
                    &mut pending_hitl,
                    &output,
                    move || {
                        reads.fetch_add(1, Ordering::Relaxed);
                        Ok(Signal::CtrlD)
                    },
                    std::future::pending::<()>(),
                )
                .await
            }
            None => return Err("sink-failure test foreground turn was not active".to_string()),
        };
        if !matches!(
            wait,
            QueuedFollowUpWait::SessionFailed(reason)
                if reason.contains("external printer closed")
        ) || read_count.load(Ordering::Relaxed) != 0
        {
            return Err("prompt sink failure did not fail the parked broker without stdin".into());
        }
        let _ = cancel_and_drain_active(&control, &mut active, &output).await;
        if active.is_some()
            || control
                .snapshot(
                    echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                    "sink-failure-conversation",
                )
                .is_some()
        {
            return Err("prompt sink failure left the foreground turn active".to_string());
        }
        Ok(())
    }

    #[test]
    fn external_output_flushes_batched_and_terminal_tokens() -> Result<(), String> {
        let printer = reedline::ExternalPrinter::new(8);
        let sender = printer.sender();
        let output = ReplExternalOutput::new(move |message| sender.try_send(message).is_ok());
        if !output.print_token(&"a".repeat(160)) {
            return Err("threshold token batch was rejected".to_string());
        }
        let threshold_batch = printer
            .get_line()
            .ok_or_else(|| "threshold token batch was not flushed".to_string())?;
        if threshold_batch.chars().count() != 160 {
            return Err("threshold token batch was split unexpectedly".to_string());
        }

        if !output.print_token("terminal tail") || !output.flush_tokens() {
            return Err("terminal token tail was rejected".to_string());
        }
        if printer.get_line().as_deref() != Some("terminal tail") {
            return Err("terminal token tail was not force-flushed".to_string());
        }
        Ok(())
    }

    fn collecting_output() -> (ReplExternalOutput, Arc<std::sync::Mutex<Vec<String>>>) {
        let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&messages);
        let output = ReplExternalOutput::new(move |message| match captured.lock() {
            Ok(mut messages) => {
                messages.push(message);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "test output collector is unavailable");
                false
            }
        });
        (output, messages)
    }

    fn captured_message_count(
        messages: &Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Result<usize, String> {
        messages
            .lock()
            .map(|messages| messages.len())
            .map_err(|error| error.to_string())
    }

    #[test]
    fn terminal_outcome_projection_does_not_duplicate_stream_terminal() -> Result<(), String> {
        use echo_agent_app_core::chat_driver::TurnOutcome;

        let (cancel_output, cancel_messages) = collecting_output();
        let cancel_sink = ReplChatSink::new(cancel_output, crate::output::OutputConfig::default());
        let mut cancel_state = ReplRenderState::default();
        if !cancel_sink.render_agent_event(&mut cancel_state, AgentEvent::Cancelled) {
            return Err("cancelled stream terminal was rejected".to_string());
        }
        cancel_sink.project_outcome(&mut cancel_state, &Ok(TurnOutcome::Cancelled));
        if captured_message_count(&cancel_messages)? != 1 {
            return Err("cancelled terminal was projected more than once".to_string());
        }

        let (failed_output, failed_messages) = collecting_output();
        let failed_sink = ReplChatSink::new(failed_output, crate::output::OutputConfig::default());
        let mut failed_state = ReplRenderState::default();
        let failure = echo_agent::error::AgentFailure::message("test_failure", "failed");
        if !failed_sink.render_agent_event(
            &mut failed_state,
            AgentEvent::Error {
                source: "test_failure".to_string(),
                message: failure.message.clone(),
                failure: failure.clone(),
            },
        ) {
            return Err("failed stream terminal was rejected".to_string());
        }
        failed_sink.project_outcome(&mut failed_state, &Ok(TurnOutcome::Failed(failure)));
        if captured_message_count(&failed_messages)? != 1 {
            return Err("failed terminal was projected more than once".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn closed_printer_settles_foreground_as_downstream_disconnect() -> Result<(), String> {
        use echo_agent_app_core::chat_driver::{ChatSink, TurnOutcome};

        let probe_printer = reedline::ExternalPrinter::new(1);
        let probe_sender = probe_printer.sender();
        drop(probe_printer);
        let probe_output =
            ReplExternalOutput::new(move |message| probe_sender.try_send(message).is_ok());
        let probe_cancel = tokio_util::sync::CancellationToken::new();
        if !probe_output.bind_turn_cancel(probe_cancel.clone()) {
            return Err("probe output failed before its first send".to_string());
        }
        let probe_sink =
            ReplChatSink::new(probe_output.clone(), crate::output::OutputConfig::default());
        if ChatSink::on_event(
            &probe_sink,
            echo_agent_app_core::chat_driver::ChatDriverEvent::TurnStatus {
                status: "probe".to_string(),
            },
        ) || !probe_output.delivery_failed()
            || !probe_cancel.is_cancelled()
        {
            return Err("closed printer did not reject delivery and cancel its bound token".into());
        }

        let printer = reedline::ExternalPrinter::new(8);
        let sender = printer.sender();
        drop(printer);
        let output = ReplExternalOutput::new(move |message| sender.try_send(message).is_ok());

        let control = echo_agent_app_core::foreground_turn::ForegroundTurnControl::default();
        let lease = control
            .begin(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                "closed-printer-conversation",
                "closed-printer-turn",
            )
            .map_err(|error| error.to_string())?;
        let cancel = lease.cancellation_token();
        if !output.bind_turn_cancel(cancel.clone()) {
            return Err("printer failed before the first delivery attempt".to_string());
        }
        let repl_sink = Arc::new(ReplChatSink::new(
            output.clone(),
            crate::output::OutputConfig::default(),
        ));
        let sink: Arc<dyn ChatSink> = repl_sink;

        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("test")
                .with_response("answer"),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("test")
                .llm_client(llm)
                .build()
                .map_err(|error| error.to_string())?,
        );
        let resources = Arc::new(echo_agent_app_core::chat_resources::ChatResources {
            execution_scope: echo_agent_app_core::workspace::WorkspaceExecutionScope::global("."),
            pool: None,
            store: None,
            sink,
            webhook_emitter: None,
            conv_id: Some("closed-printer-conversation".to_string()),
            root_message_id: "closed-printer-turn".to_string(),
            attachments: Vec::new(),
            cancel: cancel.clone(),
            interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode::Auto,
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let turn = echo_agent_app_core::prepared_turn::PreparedUserTurn {
            instruction: "respond".to_string(),
            resources: Vec::new(),
            authorship: echo_agent_app_core::prepared_turn::InstructionAuthorship::User,
        };
        let outcome = echo_agent_app_core::foreground_turn::drive_foreground_chat(
            lease, &agent, &turn, resources,
        )
        .await?;
        if !matches!(
            outcome,
            TurnOutcome::Failed(ref failure) if failure.code == "downstream_disconnect"
        ) {
            return Err(format!("unexpected closed-printer outcome: {outcome:?}"));
        }
        if !output.delivery_failed() || !cancel.is_cancelled() {
            return Err("closed printer did not reject delivery and cancel the exact token".into());
        }
        if control
            .snapshot(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                "closed-printer-conversation",
            )
            .is_some()
        {
            return Err(
                "closed-printer failed outcome did not settle the foreground registry".into(),
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn dropping_active_turn_aborts_supervisor_and_clears_registry() -> Result<(), String> {
        let control = echo_agent_app_core::foreground_turn::ForegroundTurnControl::default();
        let lease = control
            .begin(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                "drop-conversation",
                "drop-turn",
            )
            .map_err(|error| error.to_string())?;
        let task = tokio::spawn(async move {
            let _lease = lease;
            std::future::pending::<()>().await;
            0
        });
        let active = ActiveReplTurn {
            workspace_id: "global".to_string(),
            conversation_id: "drop-conversation".to_string(),
            turn_id: "drop-turn".to_string(),
            control: control.clone(),
            task: Some(task),
            completion: None,
            cancel_on_drop: true,
        };
        drop(active);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while control
                .snapshot(
                    echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                    "drop-conversation",
                )
                .is_some()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "aborted supervisor did not settle the foreground registry".to_string())?;
        Ok(())
    }

    async fn assert_input_boundary_rejects_and_settles(reason: &str) -> Result<(), String> {
        let (provider, mut hitl_rx, _failure_rx) =
            echo_agent_app_core::hitl::ReplHumanLoopProvider::channel(Arc::new(
                |_prompt: String| Ok(()),
            ));
        let provider = Arc::new(provider);
        let mut request = HumanLoopRequest::input("confirm");
        request.request_id = Some(format!("request-{reason}"));
        let response_provider = Arc::clone(&provider);
        let response_task = tokio::spawn(async move { response_provider.request(request).await });
        let request = hitl_rx
            .recv()
            .await
            .ok_or_else(|| "HITL request was not queued".to_string())?;
        let mut pending_hitl = VecDeque::from([request]);

        let control = echo_agent_app_core::foreground_turn::ForegroundTurnControl::default();
        let lease = control
            .begin(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                format!("conversation-{reason}"),
                format!("turn-{reason}"),
            )
            .map_err(|error| error.to_string())?;
        let other_lease = control
            .begin(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                format!("other-conversation-{reason}"),
                format!("other-turn-{reason}"),
            )
            .map_err(|error| error.to_string())?;
        let cancel = lease.cancellation_token();
        let task = tokio::spawn(async move {
            cancel.cancelled().await;
            lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Cancelled);
            0
        });
        let conversation_id = format!("conversation-{reason}");
        let turn_id = format!("turn-{reason}");
        let mut active = Some(ActiveReplTurn {
            workspace_id: "global".to_string(),
            conversation_id: conversation_id.clone(),
            turn_id,
            control: control.clone(),
            task: Some(task),
            completion: None,
            cancel_on_drop: true,
        });

        reject_pending_hitl(&mut pending_hitl, reason);
        let output = OutputRenderer::default();
        let _ = cancel_and_drain_active(&control, &mut active, &output).await;
        if active.is_some() {
            return Err("renderer task was not drained".to_string());
        }
        if control
            .snapshot(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                &conversation_id,
            )
            .is_some()
        {
            return Err("target foreground owner did not settle".to_string());
        }
        if control
            .snapshot(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                other_lease.conversation_id(),
            )
            .is_none()
        {
            return Err("exact cancellation affected another conversation".to_string());
        }

        let response = response_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        if !matches!(
            response,
            HumanLoopResponse::Rejected { reason: Some(actual) } if actual == reason
        ) {
            return Err(
                "pending HITL request was not rejected with the boundary reason".to_string(),
            );
        }
        drop(other_lease);
        Ok(())
    }

    #[tokio::test]
    async fn ctrl_c_eof_and_exit_reject_pending_and_wait_exact_settlement() -> Result<(), String> {
        for reason in [
            "User interrupted the active turn",
            "CLI input closed",
            "CLI session exited",
        ] {
            assert_input_boundary_rejects_and_settles(reason).await?;
        }
        Ok(())
    }
}
