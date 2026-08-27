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

struct QueuedReplTurn {
    message: String,
    interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode,
    attachments: Vec<echo_agent_app_core::attachments::AttachmentRef>,
    staged_attachment_batch: Option<echo_agent_app_core::attachments::StagedAttachmentBatch>,
    task_run_resume: Option<echo_agent_app_core::tasks::task_runtime::TaskRunResumeIdentity>,
}

enum ReplTurnStartError {
    Retryable(echo_agent_app_core::foreground_turn::ForegroundTurnError),
    WorkspaceTransition(String),
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

    fn should_retain_fifo_head(&self) -> bool {
        matches!(self, Self::Retryable(_) | Self::WorkspaceTransition(_))
    }

    fn message(&self) -> String {
        match self {
            Self::Retryable(error) => error.to_string(),
            Self::WorkspaceTransition(error) => error.clone(),
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
    resume_is_continuation: bool,
    conversation_input_attempt:
        Option<echo_agent_app_core::conversation_input::ConversationInputAttempt>,
}

struct ActiveReplTurn {
    workspace_id: String,
    execution_root: std::path::PathBuf,
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
        if let Err(error) = self.control.request_root_cancel_scoped(
            &self.workspace_id,
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
            &self.conversation_id,
            &self.turn_id,
        ) {
            tracing::debug!(%error, "dropped CLI turn could not request exact cancellation");
        }
        // Drop cannot await. Normal REPL exits use cancel_and_drain_active;
        // this defensive boundary requests root cancellation and then detaches
        // the sole outer owner so it can publish settlement after every finite
        // continuation driver has actually released.
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
            echo_agent_app_core::chat_driver::ChatDriverEvent::ExtensionReceipt(receipt) => {
                self.output.emit(receipt.display_message())
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
            echo_agent_app_core::chat_driver::ChatDriverEvent::InputLifecycle(fact) => {
                self.output.emit(format!(
                    "Conversation input {}: {}",
                    fact.identity().input_id,
                    conversation_input_fact_phase(fact.as_ref())
                ))
            }
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
            | echo_agent_app_core::chat_driver::ChatDriverEvent::AwaiterResultDeliveryStarted {
                ..
            }
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

fn conversation_input_fact_phase(
    fact: &echo_agent_app_core::conversation_input::ConversationInputFact,
) -> &'static str {
    use echo_agent_app_core::conversation_input::ConversationInputFact;
    match fact {
        ConversationInputFact::Persisted { .. } => "persisted",
        ConversationInputFact::AttemptStarted { .. } => "attempt_started",
        ConversationInputFact::MailboxAccepted { .. } => "mailbox_accepted",
        ConversationInputFact::Drained { .. } => "drained",
        ConversationInputFact::TurnSettled { .. } => "turn_settled",
        ConversationInputFact::Deferred { .. } => "deferred",
        ConversationInputFact::Reordered { .. } => "reordered",
        ConversationInputFact::RecoveryRequired { .. } => "recovery_required",
        ConversationInputFact::Cancelled { .. } => "cancelled",
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
    Rejected,
}

struct ReplInputSubmission {
    address: echo_agent_app_core::conversation_input::ConversationInputAddress,
    cleanup_warning: Option<String>,
}

enum ReplInputSubmitFailure {
    BeforePersist(String),
    AfterSubmit(String),
}

impl ReplInputSubmitFailure {
    fn can_restore_staging(&self) -> bool {
        matches!(self, Self::BeforePersist(_))
    }
}

impl std::fmt::Display for ReplInputSubmitFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforePersist(error) | Self::AfterSubmit(error) => formatter.write_str(error),
        }
    }
}

struct ReplProjectionAttachments {
    refs: Vec<echo_agent_app_core::attachments::AttachmentRef>,
    batch: Option<echo_agent_app_core::attachments::StagedAttachmentBatch>,
}

struct ReplProjectionResources {
    prepared: Option<echo_agent_app_core::prepared_turn::PreparedUserTurn>,
    spill_dir: std::path::PathBuf,
    staged_attachment_batch: Option<echo_agent_app_core::attachments::StagedAttachmentBatch>,
}

impl ReplProjectionResources {
    fn new(
        prepared: echo_agent_app_core::prepared_turn::PreparedUserTurn,
        spill_dir: std::path::PathBuf,
        staged_attachment_batch: Option<echo_agent_app_core::attachments::StagedAttachmentBatch>,
    ) -> Self {
        Self {
            prepared: Some(prepared),
            spill_dir,
            staged_attachment_batch,
        }
    }

    fn commit(mut self) {
        if let Some(batch) = self.staged_attachment_batch.take() {
            batch.commit();
        }
        let _ = self.prepared.take();
    }

    fn rollback(mut self) -> Result<(), String> {
        let prepared_error = self
            .prepared
            .take()
            .and_then(|prepared| prepared.cleanup_resources(&self.spill_dir).err())
            .map(|error| error.to_string());
        let staged_error = self
            .staged_attachment_batch
            .take()
            .and_then(|batch| batch.rollback().err());
        match (prepared_error, staged_error) {
            (None, None) => Ok(()),
            (Some(error), None) | (None, Some(error)) => Err(error),
            (Some(prepared), Some(staged)) => {
                Err(format!("{prepared}; staging cleanup also failed: {staged}"))
            }
        }
    }
}

impl Drop for ReplProjectionResources {
    fn drop(&mut self) {
        if let Some(prepared) = self.prepared.take()
            && let Err(error) = prepared.cleanup_resources(&self.spill_dir)
        {
            tracing::error!(%error, "failed to roll back uncommitted CLI input resources");
        }
        // The staging batch is fail-closed and rolls itself back on drop.
    }
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
    /// Shared webhook emitter (built from `EkoConfig.webhooks` at bootstrap).
    /// `None` means no endpoints configured — emit calls are skipped cheaply.
    pub webhook_emitter: Option<std::sync::Arc<echo_agent_app_core::webhook::WebhookEmitter>>,
    /// Authoritative application state used by workspace and other stateful commands.
    pub app_state: Option<Arc<echo_agent_app_core::state::AppState>>,
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            prompt: "echo".to_string(),
            history_file: echo_agent_app_core::data_root::user_data_path("history.txt")
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
    let mut pending_hitl = VecDeque::new();
    let mut pending_git: Option<PendingGitAction> = None;

    let repl_result: anyhow::Result<()> = 'repl: loop {
        if active_turn.is_none() {
            start_next_durable_turn(
                &agent,
                output.as_ref(),
                live_output.clone(),
                &config,
                &mut active_turn,
                *interaction_mode.read().await,
            )
            .await;
        }
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
            start_next_durable_turn(
                &agent,
                output.as_ref(),
                live_output.clone(),
                &config,
                &mut active_turn,
                *interaction_mode.read().await,
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
            start_next_durable_turn(
                &agent,
                output.as_ref(),
                live_output.clone(),
                &config,
                &mut active_turn,
                *interaction_mode.read().await,
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
                        let mut queued = QueuedReplTurn {
                            message: line.to_string(),
                            interaction_mode: mode,
                            attachments,
                            staged_attachment_batch: None,
                            task_run_resume: None,
                        };
                        if let Some(active) = active_turn.as_ref() {
                            let disposition = route_active_input(
                                &agent,
                                active,
                                queued,
                                &config,
                                &staged_attachments,
                                live_output.clone(),
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
                                        start_next_durable_turn(
                                            &agent,
                                            output.as_ref(),
                                            live_output.clone(),
                                            &config,
                                            &mut active_turn,
                                            *interaction_mode.read().await,
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
                                        start_next_durable_turn(
                                            &agent,
                                            output.as_ref(),
                                            live_output.clone(),
                                            &config,
                                            &mut active_turn,
                                            *interaction_mode.read().await,
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
                            match submit_repl_conversation_input(
                                &config,
                                &queued.message,
                                &queued.attachments,
                            )
                            .await
                            {
                                Ok(submission) => {
                                    if let Some(warning) = submission.cleanup_warning {
                                        output.print_warning(&format!(
                                            "Input persisted, but local staging cleanup failed: {warning}"
                                        ));
                                    }
                                }
                                Err(error) => {
                                    if error.can_restore_staging() {
                                        restore_repl_staged_attachments(
                                            &staged_attachments,
                                            std::mem::take(&mut queued.attachments),
                                        )
                                        .await;
                                    }
                                    output.print_warning(&format!(
                                        "Could not persist input: {error}"
                                    ));
                                }
                            }
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
                                let mut input = QueuedReplTurn {
                                    message,
                                    interaction_mode: mode,
                                    attachments,
                                    staged_attachment_batch: None,
                                    task_run_resume: None,
                                };
                                match submit_repl_conversation_input(
                                    &config,
                                    &input.message,
                                    &input.attachments,
                                )
                                .await
                                {
                                    Ok(submission) => {
                                        if let Some(warning) = submission.cleanup_warning {
                                            output.print_warning(&format!(
                                                "Input persisted, but local staging cleanup failed: {warning}"
                                            ));
                                        }
                                        start_next_durable_turn(
                                            &agent,
                                            output.as_ref(),
                                            live_output.clone(),
                                            &config,
                                            &mut active_turn,
                                            mode,
                                        )
                                        .await;
                                    }
                                    Err(error) => {
                                        if error.can_restore_staging() {
                                            restore_repl_staged_attachments(
                                                &staged_attachments,
                                                std::mem::take(&mut input.attachments),
                                            )
                                            .await;
                                        }
                                        output.print_error(&format!(
                                            "Unable to persist CLI input: {error}"
                                        ));
                                    }
                                }
                            }
                            CommandResult::ResumeTaskRun { message, identity } => {
                                let mut input = QueuedReplTurn {
                                    message,
                                    attachments: Vec::new(),
                                    staged_attachment_batch: None,
                                    interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode::Task,
                                    task_run_resume: Some(identity),
                                };
                                let turn_id = uuid::Uuid::new_v4().to_string();
                                match prepare_repl_turn_start(
                                    &agent, &mut input, &config, turn_id, None,
                                )
                                .await
                                {
                                    Ok(prepared) => {
                                        active_turn = Some(spawn_prepared_repl_turn(
                                            &agent,
                                            input,
                                            output.as_ref(),
                                            live_output.clone(),
                                            &config,
                                            prepared,
                                        ));
                                    }
                                    Err(error) => output.print_error(&format!(
                                        "Unable to resume TaskRun: {}",
                                        error.message()
                                    )),
                                }
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
                    start_next_durable_turn(
                        &agent,
                        output.as_ref(),
                        live_output.clone(),
                        &config,
                        &mut active_turn,
                        *interaction_mode.read().await,
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
    let abandoned_attachments = {
        let mut staged = staged_attachments.lock().await;
        std::mem::take(&mut *staged)
    };
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
        eprintln!("  Auto-memory: Review integration is not configured.");
        return;
    };
    let evidence_lease = match integration.lease_generation() {
        Ok(lease) => lease,
        Err(error) => {
            eprintln!(
                "  Auto-memory: workspace is switching; candidates were not queued ({error})"
            );
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
        Ok(candidates) => eprintln!(
            "  Auto-memory: queued {} observation candidate(s) for review.",
            candidates.len()
        ),
        Err(error) => eprintln!("  Auto-memory: failed to queue candidates ({error})"),
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
    let waiter = match control.request_root_cancel_scoped(
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

async fn current_repl_input_scope(
    config: &ReplConfig,
) -> Result<
    (
        echo_agent_app_core::state::ScopedChatRuntime,
        echo_agent_app_core::conversation_input::ConversationInputAddress,
    ),
    String,
> {
    let app_state = config
        .app_state
        .as_ref()
        .ok_or_else(|| "CLI conversation input service is unavailable".to_string())?;
    let runtime = app_state
        .current_control_runtime()
        .await
        .map_err(|error| error.to_string())?;
    let conversation_id = runtime
        .primary_agent()
        .read(|agent| agent.conversation_id().map(str::to_string))
        .await
        .filter(|conversation_id| !conversation_id.trim().is_empty())
        .ok_or_else(|| "CLI conversation identity is unavailable".to_string())?;
    let address = echo_agent_app_core::conversation_input::ConversationInputAddress {
        workspace_id: runtime.execution_scope().workspace_id().to_string(),
        conversation_id,
    };
    Ok((runtime, address))
}

async fn submit_repl_conversation_input(
    config: &ReplConfig,
    message: &str,
    attachments: &[echo_agent_app_core::attachments::AttachmentRef],
) -> Result<ReplInputSubmission, ReplInputSubmitFailure> {
    let app_state = config.app_state.as_ref().ok_or_else(|| {
        ReplInputSubmitFailure::BeforePersist(
            "CLI conversation input service is unavailable".to_string(),
        )
    })?;
    let (_runtime, address) = current_repl_input_scope(config)
        .await
        .map_err(ReplInputSubmitFailure::BeforePersist)?;
    let refs_for_read = attachments.to_vec();
    let attachment_data = app_state
        .session
        .product_data_io
        .run("persist CLI conversation input attachments", move || {
            echo_agent_app_core::attachments::attachment_refs_to_data(&refs_for_read)
        })
        .await
        .map_err(|error| ReplInputSubmitFailure::BeforePersist(error.to_string()))?
        .map_err(|error| ReplInputSubmitFailure::BeforePersist(error.to_string()))?;
    let external_id = uuid::Uuid::new_v4().to_string();
    let input_id = echo_agent_app_core::conversation_input::stable_scoped_input_id(
        &address,
        echo_agent_app_core::conversation_input::ConversationInputSource::Cli,
        &external_id,
    )
    .map_err(|error| ReplInputSubmitFailure::BeforePersist(error.to_string()))?;
    let submit_result = app_state
        .conversation_inputs()
        .submit(
            address.clone(),
            input_id,
            message.to_string(),
            attachment_data,
        )
        .await;

    // Once submit has been invoked, the append outcome can be ambiguous to the
    // surface. Retire the local staging either way and never recreate a second
    // ordinary-message authority beside the durable ingress record.
    let refs_for_cleanup = attachments.to_vec();
    let cleanup_result = app_state
        .session
        .product_data_io
        .run("retire persisted CLI input staging", move || {
            echo_agent_app_core::attachments::discard_staged_attachment_refs(&refs_for_cleanup)
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result.map_err(|error| error.to_string()));
    match submit_result {
        Ok(_) => Ok(ReplInputSubmission {
            address,
            cleanup_warning: cleanup_result.err(),
        }),
        Err(error) => {
            let detail = cleanup_result.err().map_or_else(
                || error.to_string(),
                |cleanup| format!("{error}; local staging cleanup also failed: {cleanup}"),
            );
            Err(ReplInputSubmitFailure::AfterSubmit(detail))
        }
    }
}

async fn restore_repl_staged_attachments(
    staged_attachments: &Arc<
        tokio::sync::Mutex<Vec<echo_agent_app_core::attachments::AttachmentRef>>,
    >,
    attachments: Vec<echo_agent_app_core::attachments::AttachmentRef>,
) {
    if attachments.is_empty() {
        return;
    }
    staged_attachments.lock().await.extend(attachments);
}

fn stage_repl_projection_attachments(
    attachment_data: &[echo_agent_app_core::types::AttachmentData],
    execution_root: &std::path::Path,
) -> Result<ReplProjectionAttachments, String> {
    if attachment_data.is_empty() {
        return Ok(ReplProjectionAttachments {
            refs: Vec::new(),
            batch: None,
        });
    }
    let uploads_dir = echo_agent_app_core::attachments::resolve_uploads_dir(Some(execution_root));
    let saved = echo_agent_app_core::attachments::save_attachments(attachment_data, &uploads_dir)
        .map_err(|error| error.to_string())?;
    let refs = saved
        .iter()
        .map(|(path, attachment)| {
            echo_agent_app_core::attachments::AttachmentRef::from_saved(path.clone(), attachment)
        })
        .collect();
    let batch = echo_agent_app_core::attachments::StagedAttachmentBatch::from_saved(&saved);
    Ok(ReplProjectionAttachments {
        refs,
        batch: Some(batch),
    })
}

async fn repl_turn_from_projection(
    config: &ReplConfig,
    runtime: &echo_agent_app_core::state::ScopedChatRuntime,
    projection: &echo_agent_app_core::conversation_input::ConversationInputProjection,
    interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode,
) -> Result<QueuedReplTurn, String> {
    let app_state = config
        .app_state
        .as_ref()
        .ok_or_else(|| "CLI conversation input service is unavailable".to_string())?;
    let attachment_data = projection.payload.attachments.clone();
    let execution_root = runtime.execution_scope().root().to_path_buf();
    let staged = app_state
        .session
        .product_data_io
        .run("stage durable CLI conversation input", move || {
            stage_repl_projection_attachments(&attachment_data, &execution_root)
        })
        .await
        .map_err(|error| error.to_string())??;
    Ok(QueuedReplTurn {
        message: projection.payload.text.clone(),
        interaction_mode,
        attachments: staged.refs,
        staged_attachment_batch: staged.batch,
        task_run_resume: None,
    })
}

fn conversation_attempt(
    projection: &echo_agent_app_core::conversation_input::ConversationInputProjection,
) -> Result<echo_agent_app_core::conversation_input::ConversationInputAttempt, String> {
    Ok(
        echo_agent_app_core::conversation_input::ConversationInputAttempt {
            identity: projection.receipt.identity.clone(),
            attempt: projection
                .receipt
                .attempt
                .ok_or_else(|| "CLI conversation input attempt is missing".to_string())?,
            attempt_id: projection
                .receipt
                .attempt_id
                .clone()
                .ok_or_else(|| "CLI conversation input attempt id is missing".to_string())?,
            turn_id: projection
                .receipt
                .turn_id
                .clone()
                .ok_or_else(|| "CLI conversation input turn id is missing".to_string())?,
            observation: Default::default(),
        },
    )
}

async fn start_next_durable_turn(
    agent: &AgentHandle,
    output: &OutputRenderer,
    live_output: ReplExternalOutput,
    config: &ReplConfig,
    active: &mut Option<ActiveReplTurn>,
    interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode,
) {
    if active.is_some() {
        return;
    }
    let (runtime, address) = match current_repl_input_scope(config).await {
        Ok(scope) => scope,
        Err(error) => {
            output.print_warning(&format!("Queued follow-up scope is unavailable: {error}"));
            return;
        }
    };
    let turn_id = uuid::Uuid::new_v4().to_string();
    let projection = match config
        .app_state
        .as_ref()
        .map(|state| state.conversation_inputs())
    {
        Some(service) => match service.dispatch_next(&address, turn_id.clone()).await {
            Ok(Some(projection)) => projection,
            Ok(None) => return,
            Err(error) => {
                output.print_warning(&format!("Queued follow-up remains pending: {error}"));
                return;
            }
        },
        None => return,
    };
    let attempt = match conversation_attempt(&projection) {
        Ok(attempt) => attempt,
        Err(error) => {
            output.print_error(&error);
            return;
        }
    };
    let mut next =
        match repl_turn_from_projection(config, &runtime, &projection, interaction_mode).await {
            Ok(next) => next,
            Err(error) => {
                if let Some(state) = config.app_state.as_ref() {
                    let _ = state
                        .conversation_inputs()
                        .deferred(attempt, error.clone())
                        .await;
                }
                output.print_warning(&format!("Queued follow-up remains pending: {error}"));
                return;
            }
        };
    match prepare_repl_turn_start(agent, &mut next, config, turn_id, Some(attempt.clone())).await {
        Ok(prepared) => {
            *active = Some(spawn_prepared_repl_turn(
                agent,
                next,
                output,
                live_output,
                config,
                prepared,
            ));
        }
        Err(error) => {
            if let Some(state) = config.app_state.as_ref() {
                let service = state.conversation_inputs();
                if error.should_retain_fifo_head() {
                    let _ = service.deferred(attempt, error.message()).await;
                } else {
                    let _ = service.recovery_required(attempt, error.message()).await;
                }
            }
            output.print_warning(&format!(
                "Queued follow-up was not started: {}",
                error.message()
            ));
        }
    }
}

async fn route_active_input(
    agent: &AgentHandle,
    active: &ActiveReplTurn,
    mut input: QueuedReplTurn,
    config: &ReplConfig,
    staged_attachments: &Arc<
        tokio::sync::Mutex<Vec<echo_agent_app_core::attachments::AttachmentRef>>,
    >,
    live_output: ReplExternalOutput,
    output: &OutputRenderer,
) -> ActiveInputDisposition {
    let address =
        match submit_repl_conversation_input(config, &input.message, &input.attachments).await {
            Ok(submission) => {
                if let Some(warning) = submission.cleanup_warning {
                    output.print_warning(&format!(
                        "Follow-up persisted, but local staging cleanup failed: {warning}"
                    ));
                }
                submission.address
            }
            Err(error) => {
                let can_restore_staging = error.can_restore_staging();
                if can_restore_staging {
                    restore_repl_staged_attachments(
                        staged_attachments,
                        std::mem::take(&mut input.attachments),
                    )
                    .await;
                }
                output.print_warning(&format!("Could not persist follow-up: {error}"));
                return if can_restore_staging {
                    ActiveInputDisposition::Rejected
                } else {
                    ActiveInputDisposition::Queued
                };
            }
        };
    let Some(app_state) = config.app_state.as_ref() else {
        return ActiveInputDisposition::Queued;
    };
    let active_turn_id = active
        .control
        .snapshot_scoped(
            &active.workspace_id,
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
            &active.conversation_id,
        )
        .filter(|snapshot| snapshot.root_turn_id == active.turn_id)
        .map(|snapshot| snapshot.active_turn_id)
        .unwrap_or_else(|| active.turn_id.clone());
    let projection = match app_state
        .conversation_inputs()
        .dispatch_next(&address, active_turn_id.clone())
        .await
    {
        Ok(Some(projection)) => projection,
        Ok(None) => {
            let pending = app_state
                .conversation_inputs()
                .list(&address)
                .await
                .map(|frontier| frontier.items.len())
                .unwrap_or(0);
            output.print_info(&format!("Follow-up persisted; {pending} pending"));
            return ActiveInputDisposition::Queued;
        }
        Err(error) => {
            output.print_warning(&format!("Follow-up dispatch failed: {error}"));
            return ActiveInputDisposition::Queued;
        }
    };
    let attempt = match conversation_attempt(&projection) {
        Ok(attempt) => attempt,
        Err(error) => {
            output.print_error(&error);
            return ActiveInputDisposition::Queued;
        }
    };
    let runtime = match current_repl_input_scope(config).await {
        Ok((runtime, _)) => runtime,
        Err(error) => {
            let _ = app_state
                .conversation_inputs()
                .deferred(attempt, error.clone())
                .await;
            output.print_warning(&error);
            return ActiveInputDisposition::Queued;
        }
    };
    let mut durable_input = match repl_turn_from_projection(
        config,
        &runtime,
        &projection,
        input.interaction_mode,
    )
    .await
    {
        Ok(input) => input,
        Err(error) => {
            let _ = app_state
                .conversation_inputs()
                .deferred(attempt, error.clone())
                .await;
            output.print_warning(&error);
            return ActiveInputDisposition::Queued;
        }
    };
    let spill_dir = echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(
        active.execution_root.as_path(),
    ));
    let prepared = match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::prepared_turn::UserTurnInput {
            text: &durable_input.message,
            attachments: &durable_input.attachments,
            spill_dir: &spill_dir,
            conversation_id: Some(&active.conversation_id),
            turn_id: Some(&active_turn_id),
        },
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = app_state
                .conversation_inputs()
                .deferred(attempt, error.to_string())
                .await;
            output.print_warning(&format!("Could not prepare steer input: {error}"));
            return ActiveInputDisposition::Queued;
        }
    };
    let message = match prepared.to_message() {
        Ok(message) => message,
        Err(error) => {
            let cleanup = prepared.cleanup_resources(&spill_dir).err();
            let _ = app_state
                .conversation_inputs()
                .deferred(attempt, error.to_string())
                .await;
            let suffix = cleanup
                .map(|cleanup| format!("; prepared resource cleanup failed: {cleanup}"))
                .unwrap_or_default();
            output.print_warning(&format!("Could not encode steer input: {error}{suffix}"));
            return ActiveInputDisposition::Queued;
        }
    };
    let resources = ReplProjectionResources::new(
        prepared,
        spill_dir,
        durable_input.staged_attachment_batch.take(),
    );
    let observed = supervise_repl_steer_delivery(
        active,
        &active_turn_id,
        app_state.conversation_inputs(),
        attempt,
        resources,
        live_output,
        || agent.steer_input_tracked(Some(&active_turn_id), message),
    )
    .await;
    match observed {
        Ok(true) => {
            output.print_info(&format!("Guidance accepted for turn {active_turn_id}"));
            ActiveInputDisposition::Steered
        }
        Ok(false) => ActiveInputDisposition::Queued,
        Err(error) => {
            output.print_warning(&format!("Steer receipt failed: {error}"));
            ActiveInputDisposition::Queued
        }
    }
}

async fn supervise_repl_steer_delivery<Steer, SteerFuture>(
    active: &ActiveReplTurn,
    expected_turn_id: &str,
    service: echo_agent_app_core::conversation_input::ConversationInputService,
    attempt: echo_agent_app_core::conversation_input::ConversationInputAttempt,
    resources: ReplProjectionResources,
    output: ReplExternalOutput,
    steer: Steer,
) -> Result<bool, String>
where
    Steer: FnOnce() -> SteerFuture,
    SteerFuture: std::future::Future<
            Output = Result<
                echo_agent::agent::AgentSteerReceipt,
                echo_agent::agent::TurnSteerError,
            >,
        >,
{
    let observer_service = service.clone();
    let observer_attempt = attempt.clone();
    let terminal_projector =
        repl_input_terminal_projector(service.clone(), attempt.clone(), Some(output.clone()));
    let (steer_tx, steer_rx) = tokio::sync::oneshot::channel();
    let observer = async move {
        let result = steer_rx
            .await
            .map_err(|error| format!("tracked steer handoff failed: {error}"))?;
        let observed = observer_service
            .observe_steer_through_drain(observer_attempt, result)
            .await
            .map_err(|error| error.to_string())?;
        emit_repl_input_receipt(&output, &observed);
        if observed.drained {
            resources.commit();
            Ok(())
        } else {
            resources.rollback()
        }
    };
    if let Err(error) = active.control.supervise_input_lifecycle_scoped(
        &active.workspace_id,
        echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
        &active.conversation_id,
        expected_turn_id,
        observer,
        terminal_projector,
    ) {
        let detail = error.to_string();
        service
            .deferred(attempt, detail.clone())
            .await
            .map_err(|projection| {
                format!("{detail}; deferred projection also failed: {projection}")
            })?;
        return Ok(false);
    }
    let result = steer().await;
    let accepted = result.is_ok();
    steer_tx
        .send(result)
        .map_err(|_| "tracked steer observer ended before receipt handoff".to_string())?;
    Ok(accepted)
}

fn repl_input_terminal_projector(
    service: echo_agent_app_core::conversation_input::ConversationInputService,
    attempt: echo_agent_app_core::conversation_input::ConversationInputAttempt,
    output: Option<ReplExternalOutput>,
) -> echo_agent_app_core::foreground_turn::ForegroundTerminalProjector {
    Arc::new(move |outcome| {
        let service = service.clone();
        let attempt = attempt.clone();
        let output = output.clone();
        Box::pin(async move {
            let receipt = service
                .settle_attempt(&attempt, &outcome)
                .await
                .map_err(|error| error.to_string())?;
            if !receipt.duplicate
                && let Some(output) = output
            {
                emit_repl_input_receipt(&output, &receipt);
            }
            Ok(())
        })
    })
}

fn emit_repl_input_receipt(
    output: &ReplExternalOutput,
    receipt: &echo_agent_app_core::conversation_input::ConversationInputReceipt,
) {
    let _ = output.emit(format!(
        "Conversation input {}: {}",
        receipt.identity.input_id,
        conversation_input_phase_label(receipt.phase)
    ));
}

fn conversation_input_phase_label(
    phase: echo_agent_app_core::conversation_input::ConversationInputPhase,
) -> &'static str {
    use echo_agent_app_core::conversation_input::ConversationInputPhase;
    match phase {
        ConversationInputPhase::Persisted => "persisted",
        ConversationInputPhase::AttemptStarted => "attempt_started",
        ConversationInputPhase::MailboxAccepted => "mailbox_accepted",
        ConversationInputPhase::Drained => "drained",
        ConversationInputPhase::TurnSettled => "turn_settled",
        ConversationInputPhase::Deferred => "deferred",
        ConversationInputPhase::RecoveryRequired => "recovery_required",
        ConversationInputPhase::Cancelled => "cancelled",
    }
}

async fn settle_repl_planned_resume(
    lease: echo_agent_app_core::foreground_turn::ForegroundTurnLease,
    outcome: echo_agent_app_core::chat_driver::TurnOutcome,
) -> Result<echo_agent_app_core::chat_driver::TurnOutcome, String> {
    lease
        .settle_after_observers(outcome.clone())
        .await
        .map_err(|error| error.to_string())?;
    Ok(outcome)
}

fn repl_taskrun_resume_binding(
    resume: echo_agent_app_core::tasks::task_runtime::TaskRunResumeIdentity,
    turn_id: impl Into<String>,
) -> echo_agent_app_core::tasks::task_runtime::RunTurnBinding {
    echo_agent_app_core::tasks::task_runtime::RunTurnBinding::resume_expected(resume, turn_id)
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

async fn settle_repl_start_failure(
    lease: echo_agent_app_core::foreground_turn::ForegroundTurnLease,
    code: &'static str,
    detail: String,
) -> ReplTurnStartError {
    let outcome = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
        echo_agent::error::AgentFailure::message(code, detail.clone()),
    );
    match lease.settle_after_observers(outcome).await {
        Ok(_) => ReplTurnStartError::Permanent(detail),
        Err(error) => ReplTurnStartError::Permanent(format!(
            "{detail}; foreground failure settlement failed: {error}"
        )),
    }
}

/// Acquire the authoritative foreground lease and prepare an immutable turn.
async fn prepare_repl_turn_start(
    _agent: &AgentHandle,
    input: &mut QueuedReplTurn,
    config: &ReplConfig,
    turn_id: String,
    conversation_input_attempt: Option<
        echo_agent_app_core::conversation_input::ConversationInputAttempt,
    >,
) -> Result<PreparedReplTurnStart, ReplTurnStartError> {
    let app_state = config.app_state.as_ref().ok_or_else(|| {
        ReplTurnStartError::Permanent("CLI foreground turn control is unavailable".to_string())
    })?;
    let scoped_runtime =
        app_state
            .current_control_runtime()
            .await
            .map_err(|error| match error {
                echo_agent_app_core::state::ScopedControlError::WorkspaceTransition => {
                    ReplTurnStartError::WorkspaceTransition(error.to_string())
                }
                echo_agent_app_core::state::ScopedControlError::Runtime(_) => {
                    ReplTurnStartError::Permanent(error.to_string())
                }
            })?;
    let conversation_id = scoped_runtime
        .primary_agent()
        .read(|value| value.conversation_id().map(str::to_string))
        .await
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            ReplTurnStartError::Permanent(format!(
                "workspace '{}' conversation identity is unavailable",
                scoped_runtime.execution_scope().workspace_id()
            ))
        })?;
    let control = app_state.session.foreground_turns.clone();
    let lease = scoped_runtime
        .begin_turn(
            &control,
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
            &conversation_id,
            turn_id.clone(),
        )
        .await
        .map_err(ReplTurnStartError::from_conversation_admission)?;
    let resume_is_continuation = if let Some(resume) = input.task_run_resume.as_ref() {
        let validation = if resume.workspace_id != scoped_runtime.execution_scope().workspace_id() {
            Err(format!(
                "TaskRun '{}' was queued for workspace '{}', but current workspace is '{}'",
                resume.run_id,
                resume.workspace_id,
                scoped_runtime.execution_scope().workspace_id()
            ))
        } else if resume.conversation_id != conversation_id {
            Err(format!(
                "TaskRun '{}' was queued for conversation '{}', but current conversation is '{}'",
                resume.run_id, resume.conversation_id, conversation_id
            ))
        } else {
            Ok(())
        };
        if let Err(detail) = validation {
            return Err(settle_repl_start_failure(lease, "task_run_resume", detail).await);
        }
        resume.continuation_enabled
    } else {
        false
    };
    let pool_execution = match scoped_runtime.agent_for(&conversation_id).await {
        Ok(execution) => execution,
        Err(error) => {
            let detail = format!("CLI AgentPool admission failed: {error}");
            return Err(settle_repl_start_failure(lease, "agent_pool", detail).await);
        }
    };
    let spill_dir = echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(
        scoped_runtime.execution_scope().root(),
    ));
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
            return Err(settle_repl_start_failure(lease, "prepared_turn", detail).await);
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
        resume_is_continuation,
        conversation_input_attempt,
    })
}

fn spawn_prepared_repl_turn(
    _agent: &AgentHandle,
    mut input: QueuedReplTurn,
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
        resume_is_continuation,
        conversation_input_attempt,
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
        workspace_io_receipt: Some(scoped_runtime.workspace_io_receipt()),
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
    let execution_root = scoped_runtime.execution_scope().root().to_path_buf();
    let scoped_runtime_guard = scoped_runtime.clone();
    let bound_turn_id = turn_id.clone();
    let conversation_inputs = config
        .app_state
        .as_ref()
        .map(|state| state.conversation_inputs());
    let staged_attachment_batch = Arc::new(tokio::sync::Mutex::new(
        input.staged_attachment_batch.take(),
    ));
    let input_drained = Arc::new(AtomicBool::new(false));
    let durable_input_attempt = conversation_input_attempt.is_some();
    let spill_dir = echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(
        scoped_runtime.execution_scope().root(),
    ));
    let (completion_tx, completion) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = live_output.print_user_message(&input.message);
        let _ = live_output.emit("Connecting to model...");
        let result = match input.task_run_resume {
            Some(resume) if resume_is_continuation => {
                let _pool_execution = pool_execution;
                echo_agent_app_core::foreground_turn::drive_foreground_chat_turn(
                    lease,
                    &agent_owned,
                    &turn,
                    resources,
                    repl_taskrun_resume_binding(resume, bound_turn_id),
                )
                .await
            }
            Some(resume) => {
                let trace_sink =
                    echo_agent_app_core::chat_driver::subagent_trace_sink_for(&resources.sink);
                let launch = match resources.store.clone() {
                    Some(store) => {
                        echo_agent_app_core::tasks::task_runtime::launch_planned_run_resume(
                            store,
                            resume,
                            agent_owned,
                            Some(pool_execution),
                            scoped_runtime_guard.review_integration(),
                            Some(trace_sink),
                            lease.cancellation_token(),
                            Some(scoped_runtime_guard.workspace_io_invocation()),
                        )
                        .await
                        .map_err(|error| error.to_string())
                    }
                    None => Err("TaskRuntime store is unavailable".to_string()),
                };
                let turn_outcome = match launch {
                    Ok(launch) => match launch.wait().await {
                        Ok(echo_agent_app_core::tasks::task_runtime::RunOutcome::Completed) => {
                            echo_agent_app_core::chat_driver::TurnOutcome::Completed
                        }
                        Ok(echo_agent_app_core::tasks::task_runtime::RunOutcome::Cancelled) => {
                            echo_agent_app_core::chat_driver::TurnOutcome::Cancelled
                        }
                        Ok(other) => echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                            echo_agent::error::AgentFailure::message(
                                "planned_resume",
                                format!("planned resume ended with {other:?}"),
                            ),
                        ),
                        Err(error) => echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                            echo_agent::error::AgentFailure::message("planned_resume", error),
                        ),
                    },
                    Err(error) => echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message("planned_resume", error),
                    ),
                };
                settle_repl_planned_resume(lease, turn_outcome).await
            }
            None => {
                let _pool_execution = pool_execution;
                match (
                    conversation_inputs.clone(),
                    conversation_input_attempt.clone(),
                ) {
                    (Some(service), Some(attempt)) => {
                        let observer_service = service.clone();
                        let observer_attempt = attempt.clone();
                        let observer_batch = Arc::clone(&staged_attachment_batch);
                        let observer_drained = Arc::clone(&input_drained);
                        let observer_output = live_output.clone();
                        let observer: echo_agent_app_core::chat_driver::InputReceiptObserver =
                            Arc::new(move |receipt| {
                                let service = observer_service.clone();
                                let attempt = observer_attempt.clone();
                                let staged_batch = Arc::clone(&observer_batch);
                                let drained = Arc::clone(&observer_drained);
                                let output = observer_output.clone();
                                Box::pin(async move {
                                    let observed = service
                                        .observe_turn_input_through_drain(attempt, receipt)
                                        .await
                                        .map_err(|error| error.to_string())?;
                                    emit_repl_input_receipt(&output, &observed);
                                    if observed.drained {
                                        drained.store(true, Ordering::Release);
                                        if let Some(batch) = staged_batch.lock().await.take() {
                                            batch.commit();
                                        }
                                    }
                                    Ok(())
                                })
                            });
                        let terminal_service = service.clone();
                        let terminal_attempt = attempt.clone();
                        let terminal_output = live_output.clone();
                        echo_agent_app_core::foreground_turn::drive_foreground_chat_with_ingress(
                            lease,
                            &agent_owned,
                            &turn,
                            resources,
                            observer,
                            move |outcome| {
                                let service = terminal_service.clone();
                                let attempt = terminal_attempt.clone();
                                let output = terminal_output.clone();
                                async move {
                                    let receipt = service
                                        .settle_attempt(&attempt, &outcome)
                                        .await
                                        .map_err(|error| error.to_string())?;
                                    if !receipt.duplicate {
                                        emit_repl_input_receipt(&output, &receipt);
                                    }
                                    Ok(())
                                }
                            },
                        )
                        .await
                    }
                    _ => {
                        echo_agent_app_core::foreground_turn::drive_foreground_chat(
                            lease,
                            &agent_owned,
                            &turn,
                            resources,
                        )
                        .await
                    }
                }
            }
        };
        if durable_input_attempt && !input_drained.load(Ordering::Acquire) {
            if let Err(error) = turn.cleanup_resources(&spill_dir) {
                tracing::warn!(%error, "failed to clean undrained CLI initial input resources");
            }
            if let Some(batch) = staged_attachment_batch.lock().await.take()
                && let Err(error) = batch.rollback()
            {
                tracing::warn!(%error, "failed to roll back undrained CLI attachment staging");
            }
        }
        let changes = renderer.finish(&result);
        live_output.clear_turn_cancel();
        let _ = completion_tx.send(());
        changes
    });

    ActiveReplTurn {
        workspace_id,
        execution_root,
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

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Result<Self, String> {
            let path =
                std::env::temp_dir().join(format!("eko-repl-{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
            Ok(Self(path))
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_active_repl_turn(
        control: echo_agent_app_core::foreground_turn::ForegroundTurnControl,
        execution_root: std::path::PathBuf,
        workspace_id: &str,
        conversation_id: &str,
        turn_id: &str,
    ) -> ActiveReplTurn {
        ActiveReplTurn {
            workspace_id: workspace_id.to_string(),
            execution_root,
            conversation_id: conversation_id.to_string(),
            turn_id: turn_id.to_string(),
            control,
            task: None,
            completion: None,
            cancel_on_drop: false,
        }
    }

    fn test_projection_resources(
        execution_root: &std::path::Path,
        conversation_id: &str,
        turn_id: &str,
    ) -> Result<(ReplProjectionResources, std::path::PathBuf), String> {
        let attachment = echo_agent_app_core::types::AttachmentData {
            name: "guidance.txt".to_string(),
            mime_type: "text/plain".to_string(),
            data: "ZHVyYWJsZSBndWlkYW5jZQ==".to_string(),
            size: 16,
            source: echo_agent_app_core::types::AttachmentSource::Paste,
        };
        let mut staged = stage_repl_projection_attachments(&[attachment], execution_root)?;
        let spill_dir =
            echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(execution_root));
        let prepared = echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
            echo_agent_app_core::prepared_turn::UserTurnInput {
                text: "use durable guidance",
                attachments: &staged.refs,
                spill_dir: &spill_dir,
                conversation_id: Some(conversation_id),
                turn_id: Some(turn_id),
            },
        )
        .map_err(|error| error.to_string())?;
        let artifact_path = prepared
            .resources
            .first()
            .map(|resource| resource.path.clone())
            .ok_or_else(|| "test paste produced no prepared resource".to_string())?;
        Ok((
            ReplProjectionResources::new(prepared, spill_dir, staged.batch.take()),
            artifact_path,
        ))
    }

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

    #[test]
    fn durable_attachment_staging_rolls_back_earlier_item_when_later_item_fails()
    -> Result<(), String> {
        use echo_agent_app_core::types::{AttachmentData, AttachmentSource};

        let temp = TestDirectory::new("durable-attachment-rollback")?;
        let attachments = vec![
            AttachmentData {
                name: "first.txt".to_string(),
                mime_type: "text/plain".to_string(),
                data: "Zmlyc3Q=".to_string(),
                size: 5,
                source: AttachmentSource::Upload,
            },
            AttachmentData {
                name: "second.txt".to_string(),
                mime_type: "text/plain".to_string(),
                data: "not-valid-base64%%%".to_string(),
                size: 1,
                source: AttachmentSource::Upload,
            },
        ];
        if stage_repl_projection_attachments(&attachments, temp.path()).is_ok() {
            return Err("invalid later attachment unexpectedly staged".to_string());
        }
        let uploads = echo_agent_app_core::attachments::resolve_uploads_dir(Some(temp.path()));
        let remaining = if uploads.exists() {
            std::fs::read_dir(&uploads)
                .map_err(|error| error.to_string())?
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(|error| error.to_string())?
                .len()
        } else {
            0
        };
        if remaining != 0 {
            return Err(format!(
                "fail-closed durable staging retained {remaining} earlier files"
            ));
        }
        Ok(())
    }

    #[test]
    fn only_pre_submit_failure_restores_local_staging() {
        let before = ReplInputSubmitFailure::BeforePersist("before".to_string());
        let after = ReplInputSubmitFailure::AfterSubmit("after".to_string());
        assert!(before.can_restore_staging());
        assert!(!after.can_restore_staging());
    }

    #[tokio::test]
    async fn closed_registration_defers_without_steer_and_rolls_back_resources()
    -> Result<(), String> {
        use echo_agent_app_core::chat_event_log::{ChatEventLog, ChatEventRetention};
        use echo_agent_app_core::conversation_input::{
            ConversationInputAddress, ConversationInputPhase, ConversationInputService,
        };
        use echo_agent_app_core::foreground_turn::{
            ForegroundTerminalProjector, ForegroundTurnControl, ForegroundTurnSurface,
        };

        let temp = TestDirectory::new("closed-live-registration")?;
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let address = ConversationInputAddress {
            workspace_id: "workspace-closed".to_string(),
            conversation_id: "conversation-closed".to_string(),
        };
        service
            .submit(
                address.clone(),
                "closed-input".to_string(),
                "closed guidance".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let started = service
            .dispatch_next(&address, "closed-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "closed input did not dispatch".to_string())?;
        let attempt = conversation_attempt(&started)?;

        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-closed",
                ForegroundTurnSurface::Cli,
                "conversation-closed",
                "closed-turn",
            )
            .map_err(|error| error.to_string())?;
        let active = test_active_repl_turn(
            control.clone(),
            temp.path().to_path_buf(),
            "workspace-closed",
            "conversation-closed",
            "closed-turn",
        );
        let (observer_entered_tx, observer_entered_rx) = tokio::sync::oneshot::channel();
        let (observer_release_tx, observer_release_rx) = tokio::sync::oneshot::channel();
        let no_op_projector: ForegroundTerminalProjector = Arc::new(|_| Box::pin(async { Ok(()) }));
        control
            .supervise_input_lifecycle_scoped(
                "workspace-closed",
                ForegroundTurnSurface::Cli,
                "conversation-closed",
                "closed-turn",
                async move {
                    let _ = observer_entered_tx.send(());
                    observer_release_rx
                        .await
                        .map_err(|_| "observer release signal closed".to_string())?;
                    Ok(())
                },
                no_op_projector,
            )
            .map_err(|error| error.to_string())?;
        let settling = tokio::spawn(async move {
            lease
                .settle_after_observers(echo_agent_app_core::chat_driver::TurnOutcome::Completed)
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), observer_entered_rx)
            .await
            .map_err(|_| "settlement did not close observer admission".to_string())?
            .map_err(|_| "observer entered signal closed".to_string())?;

        let (resources, artifact_path) =
            test_projection_resources(temp.path(), "conversation-closed", "closed-turn")?;
        if !artifact_path.exists() {
            return Err("test resource was not prepared before registration".to_string());
        }
        let steer_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&steer_calls);
        let receipt_output = ReplExternalOutput::new(|_| true);
        let accepted = supervise_repl_steer_delivery(
            &active,
            "closed-turn",
            service.clone(),
            attempt,
            resources,
            receipt_output,
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err(echo_agent::agent::TurnSteerError::NoActiveTurn))
            },
        )
        .await?;
        if accepted || steer_calls.load(Ordering::SeqCst) != 0 {
            return Err("closed lifecycle registration still executed steer".to_string());
        }
        if artifact_path.exists() {
            return Err("closed lifecycle registration retained prepared resources".to_string());
        }
        let frontier = service
            .list(&address)
            .await
            .map_err(|error| error.to_string())?;
        if frontier.items.first().map(|item| item.receipt.phase)
            != Some(ConversationInputPhase::Deferred)
        {
            return Err("closed lifecycle registration was not canonically deferred".to_string());
        }
        observer_release_tx
            .send(())
            .map_err(|_| "observer release receiver closed".to_string())?;
        settling
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn fast_terminal_is_observed_after_registration_and_commits_resources()
    -> Result<(), String> {
        use echo_agent_app_core::chat_event_log::{ChatEventLog, ChatEventRetention};
        use echo_agent_app_core::conversation_input::{
            ConversationInputAddress, ConversationInputService,
        };
        use echo_agent_app_core::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

        let temp = TestDirectory::new("fast-live-terminal")?;
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let address = ConversationInputAddress {
            workspace_id: "workspace-fast".to_string(),
            conversation_id: "conversation-fast".to_string(),
        };
        service
            .submit(
                address.clone(),
                "fast-input".to_string(),
                "fast guidance".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let started = service
            .dispatch_next(&address, "fast-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "fast input did not dispatch".to_string())?;
        let attempt = conversation_attempt(&started)?;

        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-fast",
                ForegroundTurnSurface::Cli,
                "conversation-fast",
                "fast-turn",
            )
            .map_err(|error| error.to_string())?;
        let active = test_active_repl_turn(
            control.clone(),
            temp.path().to_path_buf(),
            "workspace-fast",
            "conversation-fast",
            "fast-turn",
        );
        let (resources, artifact_path) =
            test_projection_resources(temp.path(), "conversation-fast", "fast-turn")?;
        let (_state_tx, state_rx) =
            tokio::sync::watch::channel(echo_agent::agent::AgentSteerState::TurnSettled {
                outcome: echo_agent::agent::AgentSteerTurnOutcome::Completed,
                drained: true,
            });
        let (receipt_output, receipt_messages) = collecting_output();
        let accepted = supervise_repl_steer_delivery(
            &active,
            "fast-turn",
            service.clone(),
            attempt,
            resources,
            receipt_output,
            move || {
                std::future::ready(Ok(echo_agent::agent::AgentSteerReceipt::new(
                    "fast-steer".to_string(),
                    "fast-turn".to_string(),
                    state_rx,
                )))
            },
        )
        .await?;
        if !accepted {
            return Err("fast terminal steer was not mailbox-accepted".to_string());
        }
        lease
            .settle_after_observers(echo_agent_app_core::chat_driver::TurnOutcome::Completed)
            .await
            .map_err(|error| error.to_string())?;
        if !artifact_path.exists() {
            return Err("drained fast terminal rolled back committed resources".to_string());
        }
        let receipt_was_rendered = receipt_messages
            .lock()
            .map_err(|_| "REPL receipt output is unavailable".to_string())?
            .iter()
            .any(|message| message.contains("Conversation input fast-input: turn_settled"));
        if !receipt_was_rendered {
            return Err("async typed receipt did not reach the REPL output channel".to_string());
        }
        if !service
            .list(&address)
            .await
            .map_err(|error| error.to_string())?
            .items
            .is_empty()
        {
            return Err("fast terminal input remained dispatchable".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn planned_resume_with_live_steer_projects_terminal_before_foreground_release()
    -> Result<(), String> {
        use echo_agent_app_core::chat_event_log::{ChatEventLog, ChatEventRetention};
        use echo_agent_app_core::conversation_input::{
            ConversationInputAddress, ConversationInputService,
        };
        use echo_agent_app_core::foreground_turn::{
            ForegroundTerminalProjector, ForegroundTurnControl, ForegroundTurnSurface,
        };
        use echo_agent_app_core::tasks::task_runtime::{RunTurnOrigin, TaskRunResumeIdentity};

        let resume = TaskRunResumeIdentity {
            run_id: "taskrun-repl".to_string(),
            workspace_id: "workspace-repl".to_string(),
            conversation_id: "conversation-repl".to_string(),
            root_message_id: "taskrun-root".to_string(),
            created_at: chrono::Utc::now(),
            goal_revision: 4,
            journal_sequence: 9,
            continuation_enabled: true,
        };
        let binding = repl_taskrun_resume_binding(resume.clone(), "taskrun-active");
        assert_eq!(binding.origin, RunTurnOrigin::Resume);
        assert_eq!(binding.turn_id, "taskrun-active");
        assert_eq!(binding.expected_resume.as_ref(), Some(&resume));

        let temp = TestDirectory::new("live-terminal-before-release")?;
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let address = ConversationInputAddress {
            workspace_id: "workspace-repl".to_string(),
            conversation_id: "conversation-repl".to_string(),
        };
        service
            .submit(
                address.clone(),
                "live-input".to_string(),
                "live guidance".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let started = service
            .dispatch_next(&address, "taskrun-active".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "live input did not dispatch".to_string())?;
        let attempt = conversation_attempt(&started)?;

        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-repl",
                ForegroundTurnSurface::Cli,
                "conversation-repl",
                "taskrun-active",
            )
            .map_err(|error| error.to_string())?;
        let observer_service = service.clone();
        let observer_attempt = attempt.clone();
        let observer = async move {
            observer_service
                .mailbox_accepted(observer_attempt.clone())
                .await
                .map_err(|error| error.to_string())?;
            observer_service
                .drained(observer_attempt)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        };
        let production_projector =
            repl_input_terminal_projector(service.clone(), attempt.clone(), None);
        let (projected_tx, projected_rx) = tokio::sync::oneshot::channel();
        let projected_tx = Arc::new(std::sync::Mutex::new(Some(projected_tx)));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
        let terminal_projector: ForegroundTerminalProjector = Arc::new(move |outcome| {
            let production = production_projector.clone();
            let projected = Arc::clone(&projected_tx);
            let release = Arc::clone(&release_rx);
            Box::pin(async move {
                production(outcome).await?;
                if let Some(sender) = projected
                    .lock()
                    .map_err(|_| "terminal projection signal is unavailable".to_string())?
                    .take()
                {
                    let _ = sender.send(());
                }
                if let Some(receiver) = release.lock().await.take() {
                    receiver
                        .await
                        .map_err(|_| "terminal release signal closed".to_string())?;
                }
                Ok(())
            })
        });
        control
            .supervise_input_lifecycle_scoped(
                "workspace-repl",
                ForegroundTurnSurface::Cli,
                "conversation-repl",
                "taskrun-active",
                observer,
                terminal_projector,
            )
            .map_err(|error| error.to_string())?;
        let settling = tokio::spawn(async move {
            settle_repl_planned_resume(
                lease,
                echo_agent_app_core::chat_driver::TurnOutcome::Completed,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), projected_rx)
            .await
            .map_err(|_| "live terminal was not projected".to_string())?
            .map_err(|_| "live terminal projection signal closed".to_string())?;
        if control
            .snapshot_scoped(
                "workspace-repl",
                ForegroundTurnSurface::Cli,
                "conversation-repl",
            )
            .is_none()
        {
            return Err("foreground lease released before live terminal projection".to_string());
        }
        release_tx
            .send(())
            .map_err(|_| "terminal projector release receiver closed".to_string())?;
        let planned_outcome = settling
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        if planned_outcome != echo_agent_app_core::chat_driver::TurnOutcome::Completed {
            return Err("planned resume settlement changed its terminal outcome".to_string());
        }
        if control
            .snapshot_scoped(
                "workspace-repl",
                ForegroundTurnSurface::Cli,
                "conversation-repl",
            )
            .is_some()
        {
            return Err("foreground lease remained after live terminal projection".to_string());
        }
        if !service
            .list(&address)
            .await
            .map_err(|error| error.to_string())?
            .items
            .is_empty()
        {
            return Err("drained live input remained dispatchable after terminal".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn live_observer_failure_projects_failed_terminal_before_release() -> Result<(), String> {
        use echo_agent_app_core::chat_event_log::{ChatEventLog, ChatEventRetention};
        use echo_agent_app_core::conversation_input::{
            ConversationInputAddress, ConversationInputOutcome, ConversationInputPhase,
            ConversationInputService,
        };
        use echo_agent_app_core::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

        let temp = TestDirectory::new("live-observer-failure")?;
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let address = ConversationInputAddress {
            workspace_id: "workspace-failure".to_string(),
            conversation_id: "conversation-failure".to_string(),
        };
        service
            .submit(
                address.clone(),
                "failed-live-input".to_string(),
                "failed live guidance".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let started = service
            .dispatch_next(&address, "failed-active-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "failed live input did not dispatch".to_string())?;
        let attempt = conversation_attempt(&started)?;

        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-failure",
                ForegroundTurnSurface::Cli,
                "conversation-failure",
                "failed-active-turn",
            )
            .map_err(|error| error.to_string())?;
        control
            .supervise_input_lifecycle_scoped(
                "workspace-failure",
                ForegroundTurnSurface::Cli,
                "conversation-failure",
                "failed-active-turn",
                async { Err("injected REPL observer failure".to_string()) },
                repl_input_terminal_projector(service.clone(), attempt.clone(), None),
            )
            .map_err(|error| error.to_string())?;
        let settlement = lease
            .settle_after_observers(echo_agent_app_core::chat_driver::TurnOutcome::Completed)
            .await
            .map_err(|error| error.to_string())?;
        if !matches!(
            settlement.outcome,
            echo_agent_app_core::chat_driver::TurnOutcome::Failed(_)
        ) {
            return Err("observer failure did not replace foreground outcome".to_string());
        }
        let frontier = service
            .list(&address)
            .await
            .map_err(|error| error.to_string())?;
        let receipt = frontier
            .items
            .first()
            .map(|item| &item.receipt)
            .ok_or_else(|| "failed undrained input left no durable projection".to_string())?;
        if receipt.phase != ConversationInputPhase::TurnSettled
            || receipt.outcome != Some(ConversationInputOutcome::Failed)
            || receipt.turn_id.as_deref() != Some("failed-active-turn")
            || receipt.drained
        {
            return Err("observer failure terminal projection was not exact".to_string());
        }
        if control
            .snapshot_scoped(
                "workspace-failure",
                ForegroundTurnSurface::Cli,
                "conversation-failure",
            )
            .is_some()
        {
            return Err("observer failure released no foreground terminal".to_string());
        }
        Ok(())
    }

    #[test]
    fn input_lifecycle_render_is_a_typed_projection() {
        use echo_agent_app_core::conversation_input::{
            ConversationInputAddress, ConversationInputFact, ConversationInputIdentity,
            ConversationInputPayload,
        };
        let fact = ConversationInputFact::Persisted {
            identity: ConversationInputIdentity {
                address: ConversationInputAddress {
                    workspace_id: "workspace-cli".to_string(),
                    conversation_id: "conversation-cli".to_string(),
                },
                input_id: "cli-input".to_string(),
                revision: 1,
                payload_sha256: "hash".to_string(),
            },
            payload: ConversationInputPayload {
                text: "hello".to_string(),
                attachments: Vec::new(),
                submitted_at_ms: 1,
                payload_sha256: "hash".to_string(),
            },
        };
        assert_eq!(conversation_input_fact_phase(&fact), "persisted");
    }

    #[tokio::test]
    async fn cli_conversation_input_survives_reopen_without_local_queue() -> Result<(), String> {
        use echo_agent_app_core::chat_event_log::{ChatEventLog, ChatEventRetention};
        use echo_agent_app_core::conversation_input::{
            ConversationInputAddress, ConversationInputService, ConversationInputSource,
            stable_scoped_input_id,
        };
        let temp = TestDirectory::new("durable-input")?;
        let root = temp.path().join("cli-input-log");
        let address = ConversationInputAddress {
            workspace_id: "workspace-cli".to_string(),
            conversation_id: "conversation-cli".to_string(),
        };
        let input_id = stable_scoped_input_id(&address, ConversationInputSource::Cli, "line-1")
            .map_err(|error| error.to_string())?;
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        service
            .submit(
                address.clone(),
                input_id.clone(),
                "durable CLI follow-up".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        drop(service);

        let reopened = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let frontier = reopened
            .list(&address)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            frontier
                .items
                .first()
                .map(|item| item.receipt.identity.input_id.as_str()),
            Some(input_id.as_str())
        );
        let started = reopened
            .dispatch_next(&address, "cli-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "reopened CLI input did not dispatch".to_string())?;
        assert_eq!(started.receipt.turn_id.as_deref(), Some("cli-turn"));
        Ok(())
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
            let _ = lease
                .settle_after_observers(echo_agent_app_core::chat_driver::TurnOutcome::Completed)
                .await;
            let _ = completion_tx.send(());
            0
        });
        let mut active = Some(ActiveReplTurn {
            workspace_id: "global".to_string(),
            execution_root: std::path::PathBuf::from("."),
            conversation_id: "queued-follow-up-conversation".to_string(),
            turn_id: "queued-follow-up-turn".to_string(),
            control: control.clone(),
            task: Some(task),
            completion: Some(completion),
            cancel_on_drop: true,
        });

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
            let _ = lease
                .settle_after_observers(echo_agent_app_core::chat_driver::TurnOutcome::Cancelled)
                .await;
            let _ = completion_tx.send(());
            0
        });
        let mut active = Some(ActiveReplTurn {
            workspace_id: "global".to_string(),
            execution_root: std::path::PathBuf::from("."),
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
            let _ = lease
                .settle_after_observers(echo_agent_app_core::chat_driver::TurnOutcome::Cancelled)
                .await;
            let _ = completion_tx.send(());
            0
        });
        let mut active = Some(ActiveReplTurn {
            workspace_id: "global".to_string(),
            execution_root: std::path::PathBuf::from("."),
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
    fn extension_receipt_uses_external_output_projection() -> Result<(), String> {
        use echo_agent_app_core::chat_driver::{ChatDriverEvent, ChatSink};
        use echo_agent_app_core::extension_commands::{
            ExtensionCommandIdentity, ExtensionCommandReceipt, ExtensionKind,
        };

        let (output, messages) = collecting_output();
        let sink = ReplChatSink::new(output, crate::output::OutputConfig::default());
        let receipt = ExtensionCommandReceipt::failed(
            ExtensionKind::Hooks,
            ExtensionCommandIdentity {
                request_id: "request-1".to_string(),
                operation_id: "operation-1".to_string(),
            },
            "global",
            "fixture failure",
        );
        if !ChatSink::on_event(&sink, ChatDriverEvent::ExtensionReceipt(Box::new(receipt))) {
            return Err("Extension receipt was rejected by external output".to_string());
        }
        let rendered = messages
            .lock()
            .map_err(|error| error.to_string())?
            .first()
            .cloned()
            .ok_or_else(|| "Extension receipt emitted no external output".to_string())?;
        if !rendered.starts_with("[FAILED] Extension scope=global")
            || !rendered.contains("request_id=request-1")
        {
            return Err(format!("unexpected Extension receipt output: {rendered}"));
        }
        Ok(())
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
            workspace_io_receipt: None,
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
    async fn dropping_active_turn_requests_root_cancel_and_owner_settles_registry()
    -> Result<(), String> {
        let control = echo_agent_app_core::foreground_turn::ForegroundTurnControl::default();
        let lease = control
            .begin(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                "drop-conversation",
                "drop-turn",
            )
            .map_err(|error| error.to_string())?;
        let cancel = lease.cancellation_token();
        let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            cancel.cancelled().await;
            let _delivered = cancelled_tx.send(());
            let _released = release_rx.await;
            let _ = lease
                .settle_after_observers(echo_agent_app_core::chat_driver::TurnOutcome::Cancelled)
                .await;
            0
        });
        let active = ActiveReplTurn {
            workspace_id: "global".to_string(),
            execution_root: std::path::PathBuf::from("."),
            conversation_id: "drop-conversation".to_string(),
            turn_id: "drop-turn".to_string(),
            control: control.clone(),
            task: Some(task),
            completion: None,
            cancel_on_drop: true,
        };
        drop(active);

        tokio::time::timeout(std::time::Duration::from_secs(1), cancelled_rx)
            .await
            .map_err(|_| "dropped CLI handle did not request root cancellation".to_string())?
            .map_err(|_| "root cancellation observer ended early".to_string())?;
        let snapshot = control
            .snapshot(
                echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                "drop-conversation",
            )
            .ok_or_else(|| {
                "Drop released the foreground registry before owner settlement".to_string()
            })?;
        if !snapshot.cancellation_requested {
            return Err("Drop did not mark the foreground root cancelled".to_string());
        }
        release_tx
            .send(())
            .map_err(|_| "foreground owner release receiver closed".to_string())?;

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
        .map_err(|_| "detached owner did not settle the foreground registry".to_string())?;
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
            let _ = lease
                .settle_after_observers(echo_agent_app_core::chat_driver::TurnOutcome::Cancelled)
                .await;
            0
        });
        let conversation_id = format!("conversation-{reason}");
        let turn_id = format!("turn-{reason}");
        let mut active = Some(ActiveReplTurn {
            workspace_id: "global".to_string(),
            execution_root: std::path::PathBuf::from("."),
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
