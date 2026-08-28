//! Stage 2 — shared chat driver (极简入口).
//!
//! `drive_chat` is the single, thin entry for a chat turn across TUI / CLI
//! channel / GUI: it takes a [`PreparedUserTurn`] (instruction + input
//! resources, with long pastes spilled to a user-input artifact), collapses it
//! into one `Message` via [`PreparedUserTurn::to_message`], and streams the
//! agent's ReAct reply through a surface-specific `ChatSink`. Explicit
//! TaskRun submission/resume owns formal run creation; an ordinary chat turn
//! stays run-less until a task tool creates a formal run.
//!
//! Multimodal (images/files) is delivered via the turn's inline resources; the
//! old `(&str, Option<&Message>)` pair has been replaced by the single
//! `PreparedUserTurn`.

use echo_agent::agent::{Agent, AgentEvent, AgentHandle, EventEnvelope, EventIdentity};
use echo_agent::prelude::Message;
use echo_agent::runtime::{
    AgentTurnDriver, EventSink, SinkControl, TurnMode, TurnReceipt, TurnRequest,
};
use echo_agent::tools::TraceSinkFn;
use futures::future::BoxFuture;

pub use echo_agent::runtime::TurnOutcome;

/// Optional application observer for the framework-owned initial-input
/// receipt. The observer receives the pending receipt when the driver starts
/// and waits on its typed Accepted/Drained/TurnSettled states; it does not
/// inspect output envelopes.
pub type InputReceiptObserver = std::sync::Arc<
    dyn Fn(echo_agent::runtime::TurnInputReceipt) -> BoxFuture<'static, Result<(), String>>
        + Send
        + Sync,
>;

use crate::tasks::task_runtime::executor::ExecEvent;
#[cfg(test)]
use crate::tasks::task_runtime::turn_lifecycle::RunTurnDecision;
use crate::tasks::task_runtime::types::{
    RunTurnBinding, RunTurnOrigin, RuntimeEventKind, TurnVisibility,
};

/// Complete product event stream consumed by every interactive surface.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "ChatAttachmentDescriptor")]
pub struct ChatAttachmentDescriptor {
    pub name: String,
    pub mime_type: String,
    pub source: crate::types::AttachmentSource,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "source", content = "event", rename_all = "snake_case")]
pub enum ChatDriverEvent {
    Agent(Box<EventEnvelope>),
    Execution(ExecEvent),
    TurnStatus {
        status: String,
    },
    ExecutionPath {
        observed_path: String,
    },
    TurnConfiguration {
        permission_mode: String,
        approval_policy: String,
        attachments: Vec<ChatAttachmentDescriptor>,
    },
    Interrupt {
        run_id: String,
        goal: String,
        new_message: String,
    },
    /// Typed durable ingress fact folded by the existing ChatEventLog reducer.
    InputLifecycle(Box<crate::conversation_input::ConversationInputFact>),
    ApprovalRequest {
        request_id: String,
        tool_name: String,
        args: serde_json::Value,
        prompt: String,
    },
    InputRequest {
        request_id: String,
        prompt: String,
    },
    SelectionRequest {
        request_id: String,
        prompt: String,
        options: Vec<String>,
        task_id: Option<String>,
        context: Option<serde_json::Value>,
        phase: Option<String>,
    },
    ContextCompressed {
        before_count: usize,
        after_count: usize,
        before_tokens: usize,
        after_tokens: usize,
    },
    /// Typed result of an app-core Extension management command.
    ExtensionReceipt(Box<crate::extension_commands::ExtensionCommandReceipt>),
    CommandCellStarted {
        cell: Box<crate::tasks::task_runtime::BackgroundCellState>,
    },
    CommandCellSettled {
        cell: Box<crate::tasks::task_runtime::BackgroundCellState>,
    },
    AwaiterResultReady {
        result: Box<crate::tasks::task_runtime::command_cells::AwaiterResult>,
    },
    AwaiterResultDeliveryStarted {
        acknowledgement: crate::tasks::task_runtime::command_cells::AwaiterResultAcknowledgement,
    },
    AwaiterResultAcknowledged {
        acknowledgement: crate::tasks::task_runtime::command_cells::AwaiterResultAcknowledgement,
    },
}

/// Bounded control-plane result for one finite Agent invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTurnOutcome {
    pub turn_id: String,
    pub terminal: TurnOutcome,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub compaction_count: u32,
    pub elapsed_seconds: u64,
    pub final_answer: Option<String>,
    pub final_message_id: Option<String>,
}

impl ChatTurnOutcome {
    fn failed(turn_id: String, failure: echo_agent::error::AgentFailure) -> Self {
        Self {
            turn_id,
            terminal: TurnOutcome::Failed(failure),
            input_tokens: 0,
            output_tokens: 0,
            compaction_count: 0,
            elapsed_seconds: 0,
            final_answer: None,
            final_message_id: None,
        }
    }
}

/// Surface event consumer for the shared chat driver.
///
/// Each surface provides one exhaustive product-event entry point:
/// - GUI (`TauriChatSink`, in the Tauri layer) emits Tauri events.
/// - TUI/CLI render the stream to the terminal.
/// - channel aggregates by sentence + forwarding.
pub trait ChatSink: Send + Sync + 'static {
    /// Return `false` to stop the current stream because the consumer closed.
    fn on_event(&self, event: ChatDriverEvent) -> bool;

    /// Receive an event only after the ordinary-chat journal has accepted it.
    /// Text surfaces may render the payload, while GUI keeps the envelope's
    /// identity and cursor on its canonical wire.
    fn on_journaled_event(&self, envelope: crate::chat_event_log::ChatEventEnvelope) -> bool {
        self.on_event(envelope.payload)
    }

    /// Receive a secondary tool detail only after the shared app-core
    /// projector has committed it. Surface adapters must not persist it again.
    fn on_tool_execution_projection(
        &self,
        _update: &crate::tool_execution_projection::ToolExecutionProjectionUpdate,
    ) -> bool {
        true
    }

    fn delivery_guarantee(&self) -> crate::chat_event_log::ChatDeliveryGuarantee {
        crate::chat_event_log::ChatDeliveryGuarantee::BestEffort
    }

    /// Return a sink safe to retain beyond the current foreground lease. Most
    /// sinks are already durable and use the default. Foreground cancellation
    /// wrappers expose their underlying sink so later finite turns do not keep
    /// a stale cancellation token or terminal-delivery latch.
    fn continuation_sink(&self) -> Option<std::sync::Arc<dyn ChatSink>> {
        None
    }

    /// Return a sink that is safe to retain after the foreground operation has
    /// settled. Deferred TaskRuns may wake later, so they keep durable journal
    /// projection while releasing surface renderers and their channels.
    fn deferred_continuation_sink(&self) -> Option<std::sync::Arc<dyn ChatSink>> {
        None
    }
}

/// Build the EKO TaskRuntime sink carried through task-local run context.
pub fn subagent_trace_sink_for(
    sink: &std::sync::Arc<dyn ChatSink>,
) -> crate::tasks::task_runtime::task_tools::TraceSink {
    let sink = std::sync::Arc::clone(sink);
    std::sync::Arc::new(move |event| {
        let _ = sink.on_event(ChatDriverEvent::Execution(event));
    })
}

/// Bridge the framework's JSON trace transport into the same product stream.
pub fn framework_trace_sink_for(sink: &std::sync::Arc<dyn ChatSink>) -> TraceSinkFn {
    let sink = std::sync::Arc::clone(sink);
    std::sync::Arc::new(
        move |value| match serde_json::from_value::<ExecEvent>(value) {
            Ok(event) => {
                let _ = sink.on_event(ChatDriverEvent::Execution(event));
            }
            Err(error) => {
                tracing::warn!(%error, "invalid TaskRuntime trace event");
            }
        },
    )
}

struct WebhookTurnObserver {
    emitter: Option<std::sync::Arc<crate::webhook::WebhookEmitter>>,
    model: String,
    tools: std::collections::HashMap<String, (String, String, std::time::Instant)>,
}

impl WebhookTurnObserver {
    fn new(emitter: Option<std::sync::Arc<crate::webhook::WebhookEmitter>>, model: String) -> Self {
        Self {
            emitter,
            model,
            tools: std::collections::HashMap::new(),
        }
    }

    fn observe(&mut self, event: &EventEnvelope) {
        let payload = &event.payload;
        let Some(emitter) = self.emitter.as_ref() else {
            return;
        };
        match payload {
            AgentEvent::LlmUsage { .. } => {}
            AgentEvent::ToolCall {
                call_id,
                invocation,
            } => {
                let args_summary = invocation
                    .args
                    .to_string()
                    .chars()
                    .take(240)
                    .collect::<String>();
                self.tools.insert(
                    call_id.clone(),
                    (
                        invocation.name.clone(),
                        args_summary,
                        std::time::Instant::now(),
                    ),
                );
            }
            AgentEvent::ToolResult {
                call_id,
                name,
                result,
            } => {
                let (tool_name, args_summary, started) = self
                    .tools
                    .remove(call_id)
                    .unwrap_or_else(|| (name.clone(), String::new(), std::time::Instant::now()));
                if result.success {
                    emitter.emit(crate::webhook::WebhookEvent::ToolCalled {
                        name: tool_name,
                        args_summary,
                        elapsed_ms: duration_millis(started.elapsed()),
                    });
                } else {
                    emitter.emit(crate::webhook::WebhookEvent::ToolFailed {
                        name: tool_name,
                        error: result
                            .error
                            .clone()
                            .unwrap_or_else(|| result.output.clone()),
                    });
                }
            }
            AgentEvent::Error { message, .. } => {
                emitter.emit(crate::webhook::WebhookEvent::AgentError {
                    error: message.clone(),
                });
            }
            AgentEvent::FinalAnswer(_) | AgentEvent::ContextCompressed { .. } => {}
            _ => {}
        }
    }

    fn finish(self, receipt: &TurnReceipt, terminal: TurnOutcome) -> ChatTurnOutcome {
        if self.should_emit_chat_completed(receipt, &terminal)
            && let Some(emitter) = self.emitter
        {
            emitter.emit(crate::webhook::WebhookEvent::ChatCompleted {
                model: self.model,
                input_tokens: usize::try_from(receipt.prompt_tokens).unwrap_or(usize::MAX),
                output_tokens: usize::try_from(receipt.completion_tokens).unwrap_or(usize::MAX),
                elapsed_ms: duration_millis(receipt.elapsed),
            });
        }
        ChatTurnOutcome {
            turn_id: receipt.turn_id.to_string(),
            terminal,
            input_tokens: receipt.prompt_tokens,
            output_tokens: receipt.completion_tokens,
            compaction_count: u32::try_from(receipt.compaction_count).unwrap_or(u32::MAX),
            elapsed_seconds: duration_seconds_rounded_up(receipt.elapsed),
            final_answer: receipt
                .final_answer
                .as_deref()
                .map(|answer| answer.chars().take(4_000).collect()),
            final_message_id: receipt.final_message_id.as_ref().map(ToString::to_string),
        }
    }

    fn should_emit_chat_completed(&self, receipt: &TurnReceipt, terminal: &TurnOutcome) -> bool {
        receipt.final_answer.is_some() && matches!(terminal, TurnOutcome::Completed)
    }
}

fn effective_terminal_after_input_observer(
    framework_terminal: TurnOutcome,
    observer_result: Result<(), String>,
) -> TurnOutcome {
    observer_result.map_or_else(
        |error| {
            TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                "input_observer",
                format!("input receipt observer failed: {error}"),
            ))
        },
        |_| framework_terminal,
    )
}

fn duration_millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn duration_seconds_rounded_up(duration: std::time::Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
}

/// Drive a chat turn through the single shared path (极简入口).
///
/// Wraps `message` (plus optional `multimodal`) into one `Message`, streams the
/// agent's reply through `sink`, and returns. No route pre-judgment; the
/// turn id is the shared context anchor for task tools and forked subagents.
/// Explicit TaskRun bindings are created by task submission/resume paths; an
/// ordinary chat turn remains run-less until a task tool creates a formal run.
///
/// ## turn/run identity
///
/// 普通 chat 轮次使用 `res.root_message_id` 作 turn_id。Task tools
/// 从该 turn_id 派生独立的 `taskrun:<turn_id>`，并创建正式 TaskRun。这样
/// 主 agent 在 ReAct 循环里调
/// `task_create` / `task_execute` / `create_complex_task` 等依赖
/// `require_run_id()` 的工具时,能从 task_local 读到 run_id,不再被
/// `"no active run — run_id not set in context"` 提前拒绝(对齐 Claude Code
/// 的无门槛只读 dispatch)。
///
/// turn_id 进入 Agent ExternalRunContext；普通聊天不写 TaskRuntimeStore。
/// `create_complex_task` 和 inline/formal plan 各自拥有真正的 run_id。
pub async fn drive_chat(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
) -> Result<TurnOutcome, String> {
    drive_chat_turn(agent, turn, res, None)
        .await
        .map(|outcome| outcome.terminal)
}

/// Drive one finite invocation with an optional existing TaskRun binding.
/// This is the only detailed driver; [`drive_chat`] is the surface-compatible
/// terminal projection of the same path.
pub async fn drive_chat_turn(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
) -> Result<ChatTurnOutcome, String> {
    drive_chat_turn_with_input_observer(agent, turn, res, binding, None).await
}

pub async fn drive_chat_turn_with_input_observer(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
    input_observer: Option<InputReceiptObserver>,
) -> Result<ChatTurnOutcome, String> {
    wait_for_previous_continuation_driver(&res, binding.as_ref()).await?;
    match prepare_chat_execution(turn, res, binding).await? {
        ChatExecutionPreparation::Ready(prepared) => {
            drive_prepared_chat(agent.clone(), turn, *prepared, None, input_observer).await
        }
        ChatExecutionPreparation::Settled(outcome) => Ok(outcome),
    }
}

/// Drive one top-level pooled conversation without reversing the canonical
/// `TaskRuntime -> Memory -> pool` acquisition order.
///
/// The returned pool lease is retained by the existing TaskRun supervisor when
/// a store is configured. No second lifecycle owner is introduced here.
pub async fn drive_pooled_chat<Configure, ConfigureFuture>(
    pool: std::sync::Arc<crate::agent_pool::AgentPool>,
    pool_key: &str,
    configure: Configure,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
) -> Result<TurnOutcome, String>
where
    Configure: FnOnce(AgentHandle) -> ConfigureFuture,
    ConfigureFuture: std::future::Future<Output = Result<(), String>>,
{
    drive_pooled_chat_turn(pool, pool_key, configure, turn, res, None)
        .await
        .map(|outcome| outcome.terminal)
}

/// Drive one finite pooled invocation with an explicit TaskRun binding.
/// Long-horizon continuation uses a run-scoped pool key so it never keeps the
/// user's foreground conversation agent locked between turns.
pub(crate) async fn drive_pooled_chat_turn<Configure, ConfigureFuture>(
    pool: std::sync::Arc<crate::agent_pool::AgentPool>,
    pool_key: &str,
    configure: Configure,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: impl Into<Option<RunTurnBinding>>,
) -> Result<ChatTurnOutcome, String>
where
    Configure: FnOnce(AgentHandle) -> ConfigureFuture,
    ConfigureFuture: std::future::Future<Output = Result<(), String>>,
{
    let binding = binding.into();
    wait_for_previous_continuation_driver(&res, binding.as_ref()).await?;
    let prepared = match prepare_chat_execution(turn, res, binding).await? {
        ChatExecutionPreparation::Ready(prepared) => prepared,
        ChatExecutionPreparation::Settled(outcome) => return Ok(outcome),
    };
    let acquire_cancel = prepared.cancel.clone();
    let execution = match tokio::select! {
        biased;
        _ = acquire_cancel.cancelled() => {
            prepared
                .reject_before_driver_start("continuation cancelled during pool admission")
                .await?;
            return Err("continuation cancelled during pool admission".to_string());
        }
        result = pool.acquire(pool_key) => result,
    } {
        Ok(execution) => execution,
        Err(error) => {
            let message = format!("AgentPool admission failed: {error}");
            prepared.reject_before_driver_start(&message).await?;
            return Err(message);
        }
    };
    let agent = execution.agent();
    let configure_cancel = prepared.cancel.clone();
    let configuration = tokio::select! {
        biased;
        _ = configure_cancel.cancelled() => {
            prepared
                .reject_before_driver_start("continuation cancelled during agent configuration")
                .await?;
            return Err("continuation cancelled during agent configuration".to_string());
        }
        result = configure(agent.clone()) => result,
    };
    if let Err(error) = configuration {
        prepared.reject_before_driver_start(&error).await?;
        return Err(error);
    }
    drive_prepared_chat(agent, turn, *prepared, Some(execution), None).await
}

async fn wait_for_previous_continuation_driver(
    resources: &crate::chat_resources::ChatResources,
    binding: Option<&RunTurnBinding>,
) -> Result<(), String> {
    let Some(store) = resources.store.as_ref() else {
        return Ok(());
    };
    let explicit_resume = binding.and_then(|binding| {
        matches!(
            binding.origin,
            RunTurnOrigin::Resume | RunTurnOrigin::Recovery
        )
        .then(|| binding.run_id.clone())
        .flatten()
    });
    if let Some(run_id) = explicit_resume {
        store.wait_for_run_driver_idle(&run_id).await;
    }
    Ok(())
}

enum ChatExecutionPreparation {
    Ready(Box<PreparedChatExecution>),
    Settled(ChatTurnOutcome),
}

struct PreparedChatExecution {
    turn_id: String,
    formal_run_id: String,
    binding: RunTurnBinding,
    task_driver_registration:
        Option<crate::tasks::task_runtime::store::RegisteredRunDriver<ChatTurnOutcome>>,
    resources: std::sync::Arc<crate::chat_resources::ChatResources>,
    cancel: echo_agent::agent::CancellationToken,
    sink: std::sync::Arc<dyn ChatSink>,
    trace_sink: crate::tasks::task_runtime::task_tools::TraceSink,
    drives_task_run: bool,
    store: Option<std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    foreground_progress: Option<crate::foreground_turn::ForegroundTurnProgress>,
}

impl PreparedChatExecution {
    /// A pool/configuration rejection happens after the RunTurn claim but
    /// before model execution. Preserve the controlling intent: application
    /// shutdown is boot-recoverable and explicit cancellation is terminal.
    /// Untyped admission failures require input; only typed LLM failures enter
    /// durable provider retry.
    async fn reject_before_driver_start(mut self, detail: &str) -> Result<(), String> {
        if !self.drives_task_run {
            if let Some(registration) = self.task_driver_registration.take() {
                registration.reject(detail.to_string());
            }
            return Ok(());
        }
        let Some(store) = self.store.as_ref() else {
            if let Some(registration) = self.task_driver_registration.take() {
                registration.reject(detail.to_string());
            }
            return Ok(());
        };
        let rejection = if !store.is_run_driver_admission_open() {
            crate::tasks::task_runtime::turn_lifecycle::PreDriverRejection::Shutdown
        } else if self.cancel.is_cancelled() {
            crate::tasks::task_runtime::turn_lifecycle::PreDriverRejection::Cancelled
        } else {
            crate::tasks::task_runtime::turn_lifecycle::PreDriverRejection::Admission
        };
        let blocking = crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store.clone());
        let settlement = crate::tasks::task_runtime::turn_lifecycle::reject_before_driver_start(
            &blocking,
            store,
            &self.formal_run_id,
            &self.turn_id,
            detail,
            rejection,
        )
        .await;
        if let Some(registration) = self.task_driver_registration.take() {
            registration.reject(detail.to_string());
        }
        settlement
    }
}

async fn prepare_chat_execution(
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
) -> Result<ChatExecutionPreparation, String> {
    let foreground_progress = crate::foreground_turn::current_foreground_progress();
    if let Some(store) = res.store.as_ref() {
        let active_workspace_id = store.active_workspace_id();
        if active_workspace_id != res.execution_scope.workspace_id() {
            return Err(format!(
                "Chat execution scope {} does not match TaskRuntime workspace {}",
                res.execution_scope.workspace_id(),
                active_workspace_id
            ));
        }
    }
    // Scope a per-turn run_id so task tools (task_create /
    // task_execute / create_complex_task) can read it via require_run_id().
    // Use root_message_id (unique per turn, set by all callers); fall back to
    // a fresh uuid if a caller forgot to set it (defensive, never panics).
    let default_turn_id = if res.root_message_id.trim().is_empty() {
        tracing::warn!("drive_chat: root_message_id empty — using fallback uuid as turn_id");
        uuid::Uuid::new_v4().to_string()
    } else {
        res.root_message_id.clone()
    };
    let binding = binding.unwrap_or_else(|| RunTurnBinding {
        run_id: None,
        turn_id: default_turn_id.clone(),
        root_message_id: default_turn_id,
        origin: RunTurnOrigin::User,
        transcript_visibility: TurnVisibility::Visible,
        expected_resume: None,
    });
    if binding.turn_id.trim().is_empty() || binding.root_message_id.trim().is_empty() {
        return Err("RunTurn binding requires non-empty turn and root message ids".to_string());
    }
    if let Some(expected) = binding.expected_resume.as_ref() {
        let binding_matches = binding.origin == RunTurnOrigin::Resume
            && binding.run_id.as_deref() == Some(expected.run_id.as_str())
            && binding.root_message_id == expected.root_message_id
            && res.execution_scope.workspace_id() == expected.workspace_id
            && res.conv_id.as_deref() == Some(expected.conversation_id.as_str());
        if !binding_matches {
            return Err(format!(
                "TaskRun '{}' expected resume binding does not match its execution scope",
                expected.run_id
            ));
        }
    }
    let turn_id = binding.turn_id.clone();
    let drives_task_run = binding.run_id.is_some();
    let formal_run_id = binding.run_id.clone().unwrap_or_else(|| {
        crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&turn_id)
    });
    // Every turn that can reach task_create/task_execute is registered before
    // memory admission. An explicit binding owns the run driver; an ordinary
    // run-less turn can still create and supervise a formal TaskRun lazily.
    let mut task_driver_registration = match res.store.as_ref() {
        Some(store) => {
            let admission = store
                .reserve_run_driver_admission(formal_run_id.clone(), res.cancel.clone())
                .map_err(|error| format!("chat driver admission failed: {error}"))?;
            let generation = store
                .lease_active_workspace_generation()
                .map_err(|error| format!("task runtime generation admission failed: {error}"))?;
            let registration = if drives_task_run {
                store.register_run_driver::<ChatTurnOutcome>(admission, generation)
            } else {
                store.register_optional_run_driver::<ChatTurnOutcome>(admission, generation)
            }
            .map_err(|error| format!("chat driver registration failed: {error}"))?;
            Some(registration)
        }
        None if drives_task_run => {
            return Err("TaskRun-bound turn requires TaskRuntimeStore".to_string());
        }
        None => None,
    };
    let memory_generation = match resolve_turn_memory_generation(&res) {
        Ok(generation) => generation,
        Err(error) => {
            let message = format!("chat memory generation unavailable: {error}");
            let failure =
                echo_agent::error::AgentFailure::message("memory_generation", message.clone());
            match EventIdentity::for_chat(
                res.conv_id.clone(),
                turn_id.clone(),
                turn_id.clone(),
                None,
            )
            .and_then(|identity| {
                EventEnvelope::new(
                    &identity,
                    1,
                    None,
                    AgentEvent::Error {
                        source: "memory_generation".to_string(),
                        message: message.clone(),
                        failure: failure.clone(),
                    },
                )
            }) {
                Ok(event) => {
                    let _delivered = res.sink.on_event(ChatDriverEvent::Agent(Box::new(event)));
                }
                Err(envelope_error) => {
                    tracing::error!(%envelope_error, "failed to report memory admission failure");
                }
            }
            return Ok(ChatExecutionPreparation::Settled(ChatTurnOutcome::failed(
                turn_id, failure,
            )));
        }
    };
    let layer_manager = memory_generation
        .as_ref()
        .map(|lease| lease.create_layer_manager().map(std::sync::Arc::new))
        .transpose()
        .map_err(|error| format!("Memory layer unavailable: {error}"))?
        .or_else(|| res.layer_manager.clone());
    let res = std::sync::Arc::new(crate::chat_resources::ChatResources {
        execution_scope: res.execution_scope.clone(),
        workspace_io_receipt: res.workspace_io_receipt.clone(),
        pool: res.pool.clone(),
        store: res.store.clone(),
        sink: res.sink.clone(),
        webhook_emitter: res.webhook_emitter.clone(),
        conv_id: res.conv_id.clone(),
        root_message_id: res.root_message_id.clone(),
        attachments: res.attachments.clone(),
        cancel: res.cancel.clone(),
        review_integration: res.review_integration.clone(),
        layer_manager,
        memory_generation,
        human_loop_provider: res.human_loop_provider.clone(),
    });
    let cancel = res.cancel.clone();
    let sink = res.sink.clone();
    let trace_sink = subagent_trace_sink_for(&sink);
    let store = res.store.clone();
    if drives_task_run {
        let registration = task_driver_registration.take().ok_or_else(|| {
            "TaskRun-bound turn lost its driver registration during preparation".to_string()
        })?;
        // The raw prepared instruction is the goal. For spilled long text it is
        // the reference block, which is a better task goal than the full paste.
        let task_store = store.as_ref().ok_or_else(|| {
            "TaskRun-bound turn lost its TaskRuntimeStore during preparation".to_string()
        })?;
        let owned_run_id = formal_run_id.clone();
        let owned_conversation_id = res.conv_id.clone();
        let owned_root_message_id = binding.root_message_id.clone();
        let owned_instruction = turn.instruction.clone();
        let owned_attachments = res.attachments.clone();
        let expected_resume = binding.expected_resume.clone();
        let origin = binding.origin;
        let visibility = binding.transcript_visibility;
        let owned_turn_id = turn_id.clone();
        let owned_trace_sink = trace_sink.clone();
        let owned_store = task_store.clone();
        let claim = crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(task_store.clone())
            .run_owned("prepare and claim chat TaskRun", move || {
                let mut registration = registration;
                registration.mark_preparation_started();
                if expected_resume.is_none() {
                    if let Err(error) = ensure_bound_task_run(
                        &owned_store,
                        &owned_run_id,
                        owned_conversation_id.as_deref(),
                        &owned_root_message_id,
                        &owned_instruction,
                        &owned_attachments,
                        Some(&owned_trace_sink),
                    ) {
                        registration.fail_preparation(error.to_string());
                        return Err(error);
                    }
                    let continuation = match owned_store.get_run_state(&owned_run_id) {
                        Ok(state) => state.and_then(|state| state.continuation),
                        Err(error) => {
                            registration.fail_preparation(error.to_string());
                            return Err(error);
                        }
                    };
                    if continuation.is_none()
                        && let Err(error) = owned_store.configure_run_continuation(
                            &owned_run_id,
                            true,
                            false,
                            None,
                            None,
                        )
                    {
                        registration.fail_preparation(error.to_string());
                        return Err(error);
                    }
                }
                let claim = if let Some(expected) = expected_resume.as_ref() {
                    owned_store.resume_and_claim_run_turn_expected(
                        expected,
                        &owned_turn_id,
                        origin,
                        visibility,
                    )
                } else if origin == RunTurnOrigin::Resume {
                    Err(crate::tasks::task_runtime::StoreError::InvalidPlan(
                        "resume RunTurn requires an exact queued TaskRun identity".to_string(),
                    ))
                } else {
                    owned_store.claim_run_turn(&owned_run_id, &owned_turn_id, origin, visibility)
                };
                Ok((registration, claim.map_err(|error| error.to_string())))
            })
            .await;
        let claim = match claim {
            Ok((registration, Ok(claim))) => {
                task_driver_registration = Some(registration);
                claim
            }
            Ok((registration, Err(message))) => {
                registration.reject(message.clone());
                return Err(message);
            }
            Err(error) => {
                return Err(error.to_string());
            }
        };
        match claim {
            crate::tasks::task_runtime::store::RunTurnClaimOutcome::Started(_) => {}
            crate::tasks::task_runtime::store::RunTurnClaimOutcome::NotSubmitted(reason) => {
                if let Some(registration) = task_driver_registration.take() {
                    registration.reject(format!("RunTurn was not submitted: {reason:?}"));
                }
                return Err(format!("RunTurn was not submitted: {reason:?}"));
            }
        }
    }
    Ok(ChatExecutionPreparation::Ready(Box::new(
        PreparedChatExecution {
            turn_id,
            formal_run_id,
            binding,
            task_driver_registration,
            resources: res,
            cancel,
            sink,
            trace_sink,
            drives_task_run,
            store,
            foreground_progress,
        },
    )))
}

async fn drive_prepared_chat(
    agent: AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    mut prepared: PreparedChatExecution,
    pool_execution: Option<crate::agent_pool::AgentPoolExecutionLease>,
    input_observer: Option<InputReceiptObserver>,
) -> Result<ChatTurnOutcome, String> {
    let continuation_dispatch_owned = prepared.binding.transcript_visibility
        == TurnVisibility::Internal
        && matches!(
            prepared.binding.origin,
            RunTurnOrigin::Continuation | RunTurnOrigin::Recovery
        );
    if let Some(provider) = prepared.resources.human_loop_provider.clone() {
        agent
            .write_async(|agent| {
                Box::pin(async move {
                    agent.set_human_loop_provider_preserving_approvals(provider);
                })
            })
            .await;
    }
    if prepared.drives_task_run
        && !continuation_dispatch_owned
        && let Some(store) = prepared.store.as_ref()
    {
        crate::tasks::task_runtime::continuation::register_launcher(
            store,
            &prepared.formal_run_id,
            agent.clone(),
            prepared.resources.clone(),
            prepared.binding.root_message_id.clone(),
            prepared.foreground_progress.clone(),
        );
    }
    let mut result = if let Some(registration) = prepared.task_driver_registration.take() {
        let store = prepared
            .store
            .as_ref()
            .ok_or_else(|| "chat driver registration lost TaskRuntimeStore".to_string())?
            .clone();
        let owned_agent = agent;
        let owned_turn = turn.clone();
        let owned_resources = prepared.resources.clone();
        let owned_run_id = prepared.formal_run_id.clone();
        let owned_binding = prepared.binding.clone();
        let owned_cancel = prepared.cancel.clone();
        let owned_trace_sink = prepared.trace_sink.clone();
        let drives_task_run = prepared.drives_task_run;
        let foreground_progress = prepared.foreground_progress.clone();
        let input_observer = input_observer.clone();
        let waiter = registration.start(move |mut receipt_owner| async move {
            let driver_execution_context = receipt_owner.execution_context_id();
            if let Some(generation) = owned_resources.memory_generation.as_ref() {
                receipt_owner.retain(generation.clone());
            }
            if let Some(execution) = pool_execution {
                receipt_owner.retain(execution);
            }
            let _projection_registration =
                crate::tasks::task_runtime::compact_context::task_runtime_projection_registry()
                    .register(owned_run_id.clone(), store.clone());
            drive_registered_turn(RegisteredTurnDriver {
                agent: owned_agent,
                initial_turn: owned_turn,
                resources: owned_resources,
                run_id: owned_run_id,
                binding: owned_binding,
                cancel: owned_cancel,
                trace_sink: owned_trace_sink,
                drives_task_run,
                store,
                driver_execution_context,
                foreground_progress,
                input_observer,
            })
            .await
        });
        waiter
            .await
            .map_err(|error| format!("chat driver result waiter failed: {error}"))?
    } else {
        let _pool_execution = pool_execution;
        let _projection_registration = prepared.store.as_ref().map(|store| {
            crate::tasks::task_runtime::compact_context::task_runtime_projection_registry()
                .register(prepared.formal_run_id.clone(), std::sync::Arc::clone(store))
        });
        crate::tasks::task_runtime::task_tools::with_run_context(
            prepared.formal_run_id.clone(),
            prepared.cancel.clone(),
            Some(prepared.trace_sink.clone()),
            drive_chat_inner(
                &agent,
                turn,
                prepared.resources.clone(),
                ChatTurnModelScope {
                    turn_id: prepared.turn_id.clone(),
                    bound_run_id: None,
                    driver_execution_context: None,
                    origin: prepared.binding.origin,
                    transcript_visibility: TurnVisibility::Visible,
                },
                input_observer,
            ),
        )
        .await
    };
    if prepared.drives_task_run
        && !continuation_dispatch_owned
        && let Some(store) = prepared.store.as_ref()
    {
        let outcome = crate::tasks::task_runtime::continuation::request_continue(
            store,
            &prepared.formal_run_id,
            RunTurnOrigin::Continuation,
        );
        if let crate::tasks::task_runtime::continuation::ContinueRequestOutcome::Running(request) =
            outcome
        {
            tracing::debug!(
                run_id = %prepared.formal_run_id,
                disposition = ?request.disposition,
                "finite RunTurn requested continuation"
            );
            if prepared.foreground_progress.is_some() {
                let completion = request.completion.wait().await?;
                if completion.terminal != TurnOutcome::Completed
                    && let Ok(outcome) = result.as_mut()
                {
                    outcome.terminal = completion.terminal;
                }
                tracing::debug!(
                    run_id = %prepared.formal_run_id,
                    reason = ?completion.reason,
                    "foreground continuation chain settled"
                );
            }
        } else {
            tracing::debug!(
                run_id = %prepared.formal_run_id,
                ?outcome,
                "finite RunTurn has no continuation launcher"
            );
            if prepared.foreground_progress.is_some() {
                return Err(format!(
                    "foreground TaskRun {} lost its continuation launcher",
                    prepared.formal_run_id
                ));
            }
        }
    }
    let observed_path = observe_execution_path(
        prepared.store.as_ref(),
        &prepared.formal_run_id,
        &prepared.turn_id,
    )
    .await;
    let _ = prepared.sink.on_event(ChatDriverEvent::ExecutionPath {
        observed_path: observed_path.to_string(),
    });
    tracing::info!(
        observed_path,
        turn_id = %prepared.turn_id,
        "chat execution path observed"
    );
    result
}

struct RegisteredTurnDriver {
    agent: AgentHandle,
    initial_turn: crate::prepared_turn::PreparedUserTurn,
    resources: std::sync::Arc<crate::chat_resources::ChatResources>,
    run_id: String,
    binding: RunTurnBinding,
    cancel: echo_agent::agent::CancellationToken,
    trace_sink: crate::tasks::task_runtime::task_tools::TraceSink,
    drives_task_run: bool,
    store: std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
    driver_execution_context: String,
    foreground_progress: Option<crate::foreground_turn::ForegroundTurnProgress>,
    input_observer: Option<InputReceiptObserver>,
}

async fn drive_registered_turn(driver: RegisteredTurnDriver) -> Result<ChatTurnOutcome, String> {
    if let Some(progress) = driver.foreground_progress.as_ref() {
        progress.advance(&driver.binding.turn_id);
    }
    let result = crate::tasks::task_runtime::task_tools::with_run_context(
        driver.run_id.clone(),
        driver.cancel.clone(),
        Some(driver.trace_sink.clone()),
        drive_chat_inner(
            &driver.agent,
            &driver.initial_turn,
            driver.resources.clone(),
            ChatTurnModelScope {
                turn_id: driver.binding.turn_id.clone(),
                bound_run_id: driver.drives_task_run.then(|| driver.run_id.clone()),
                driver_execution_context: Some(driver.driver_execution_context.clone()),
                origin: driver.binding.origin,
                transcript_visibility: driver.binding.transcript_visibility,
            },
            driver.input_observer,
        ),
    )
    .await;
    if !driver.drives_task_run {
        return result;
    }
    let fallback;
    let outcome = match result.as_ref() {
        Ok(outcome) => outcome,
        Err(error) => {
            fallback = ChatTurnOutcome::failed(
                driver.binding.turn_id,
                echo_agent::error::AgentFailure::message("chat_driver", error.clone()),
            );
            &fallback
        }
    };
    let _decision = finalize_run_turn(
        &driver.store,
        &driver.run_id,
        outcome,
        Some(&driver.trace_sink),
    )
    .await?;
    result
}

fn resolve_turn_memory_generation(
    resources: &crate::chat_resources::ChatResources,
) -> Result<Option<crate::evolution::ReviewGenerationLease>, crate::evolution::ReviewGenerationError>
{
    if let Some(generation) = resources.memory_generation.clone() {
        return Ok(Some(generation));
    }
    resources
        .review_integration
        .as_ref()
        .map(|integration| integration.lease_generation())
        .transpose()
}

fn ensure_bound_task_run(
    store: &crate::tasks::task_runtime::TaskRuntimeStore,
    run_id: &str,
    conversation_id: Option<&str>,
    turn_id: &str,
    goal: &str,
    attachments: &[crate::attachments::AttachmentRef],
    trace_sink: Option<&crate::tasks::task_runtime::task_tools::TraceSink>,
) -> Result<(), crate::tasks::task_runtime::StoreError> {
    use crate::tasks::task_runtime::{AttendedMode, DomainProfile, TaskRunStatus};

    let run = store.create_run_for_active_workspace(
        run_id,
        conversation_id.unwrap_or("message:task"),
        turn_id,
        DomainProfile::General,
        goal,
        "agent_task_plan",
        AttendedMode::Attended,
    )?;
    if !attachments.is_empty() {
        store.set_run_attachments(run_id, attachments)?;
    }
    if run.status == TaskRunStatus::Pending {
        store.transition_run(run_id, TaskRunStatus::Running)?;
        if let Some(trace_sink) = trace_sink {
            trace_sink(ExecEvent::run(
                run.workspace_id.clone(),
                run.conversation_id.clone(),
                run_id.to_string(),
                RuntimeEventKind::RunStarted,
                serde_json::json!({
                    "conversation_id": conversation_id,
                    "goal": goal,
                    "route": "agent_task_plan",
                }),
            ));
        }
    }
    Ok(())
}

async fn finalize_run_turn(
    store: &std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
    run_id: &str,
    outcome: &ChatTurnOutcome,
    trace_sink: Option<&crate::tasks::task_runtime::task_tools::TraceSink>,
) -> Result<crate::tasks::task_runtime::turn_lifecycle::RunTurnDecision, String> {
    let blocking = crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store.clone());
    crate::tasks::task_runtime::turn_lifecycle::finalize_run_turn(
        &blocking,
        store,
        run_id,
        &crate::tasks::task_runtime::turn_lifecycle::RunTurnTerminal {
            turn_id: &outcome.turn_id,
            terminal: &outcome.terminal,
            elapsed_seconds: outcome.elapsed_seconds,
            final_message_id: outcome.final_message_id.as_deref(),
        },
        trace_sink,
    )
    .await
}

async fn observe_execution_path(
    store: Option<&std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    formal_run_id: &str,
    turn_id: &str,
) -> &'static str {
    use crate::tasks::task_runtime::TaskRunStatus;

    let Some(store) = store else {
        return "direct";
    };
    let formal_run_id = formal_run_id.to_string();
    let turn_id = turn_id.to_string();
    crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store.clone())
        .run_store("observe chat execution path", move |store| {
            if let Some(run) = store.get_run(&formal_run_id)? {
                let observed = if run.route == "agent_autonomous" {
                    "detached_background"
                } else {
                    "formal_plan"
                };
                store.record_execution_path(&run.run_id, observed)?;
                return Ok(observed);
            }
            let statuses = [
                TaskRunStatus::Pending,
                TaskRunStatus::Running,
                TaskRunStatus::Paused,
                TaskRunStatus::Cancelled,
                TaskRunStatus::Failed,
                TaskRunStatus::Completed,
            ];
            let matching = store
                .list_runs_in(&statuses)?
                .into_iter()
                .filter(|run| run.root_message_id == turn_id)
                .collect::<Vec<_>>();
            let observed = if matching.iter().any(|run| run.run_id == formal_run_id) {
                "formal_plan"
            } else if matching.iter().any(|run| run.route == "agent_autonomous") {
                "detached_background"
            } else {
                "direct"
            };
            for run in matching {
                store.record_execution_path(&run.run_id, observed)?;
            }
            Ok(observed)
        })
        .await
        .unwrap_or("direct")
}

struct ChatTurnModelScope {
    turn_id: String,
    bound_run_id: Option<String>,
    driver_execution_context: Option<String>,
    origin: RunTurnOrigin,
    transcript_visibility: TurnVisibility,
}

struct EkoTurnEventSink {
    state: std::sync::Arc<std::sync::Mutex<EkoTurnEventSinkState>>,
    sender: tokio::sync::mpsc::Sender<EkoTurnProjectionRequest>,
    _projector_task: tokio::task::JoinHandle<()>,
}

struct EkoTurnEventSinkState {
    webhook_observer: Option<WebhookTurnObserver>,
    expose_internal_synthesis: bool,
    downstream_failure: Option<echo_agent::error::AgentFailure>,
}

enum EkoTurnProjectionRequest {
    Event {
        event: Box<EventEnvelope>,
        acknowledgement: tokio::sync::oneshot::Sender<echo_agent::error::Result<SinkControl>>,
    },
    #[cfg(test)]
    Stop {
        acknowledgement: tokio::sync::oneshot::Sender<()>,
    },
}

impl EkoTurnEventSink {
    fn new(
        sink: std::sync::Arc<dyn ChatSink>,
        webhook_observer: WebhookTurnObserver,
        active_run_id: Option<String>,
        runtime_store: Option<std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
        turn_id: String,
        transcript_visibility: TurnVisibility,
    ) -> Self {
        let state = std::sync::Arc::new(std::sync::Mutex::new(EkoTurnEventSinkState {
            webhook_observer: Some(webhook_observer),
            expose_internal_synthesis: transcript_visibility == TurnVisibility::Visible,
            downstream_failure: None,
        }));
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let projector_state = std::sync::Arc::clone(&state);
        let projector_store = runtime_store.clone();
        let owner_store = runtime_store;
        let projector = async move {
            while let Some(request) = receiver.recv().await {
                match request {
                    EkoTurnProjectionRequest::Event {
                        event,
                        acknowledgement,
                    } => {
                        let result = project_eko_turn_event(
                            &sink,
                            &projector_state,
                            active_run_id.as_deref(),
                            projector_store.as_ref(),
                            &turn_id,
                            *event,
                        )
                        .await;
                        let _ = acknowledgement.send(result);
                    }
                    #[cfg(test)]
                    EkoTurnProjectionRequest::Stop { acknowledgement } => {
                        let _ = acknowledgement.send(());
                        break;
                    }
                }
            }
            Ok::<(), crate::tasks::task_runtime::StoreError>(())
        };
        let failure_state = std::sync::Arc::clone(&state);
        let projector_task = if let Some(store) = owner_store {
            tokio::spawn(async move {
                if let Err(error) =
                    crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store)
                        .run_async_owned("drive EKO turn projector", projector)
                        .await
                    && let Ok(mut state) = failure_state.lock()
                {
                    state.downstream_failure = Some(echo_agent::error::AgentFailure::message(
                        "sink_projector_failed",
                        error.to_string(),
                    ));
                }
            })
        } else {
            tokio::spawn(async move {
                let _ = projector.await;
            })
        };
        Self {
            state,
            sender,
            _projector_task: projector_task,
        }
    }

    fn finish(
        &self,
        receipt: TurnReceipt,
        framework_terminal: TurnOutcome,
    ) -> Result<ChatTurnOutcome, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let terminal = state
            .downstream_failure
            .take()
            .map(TurnOutcome::Failed)
            .unwrap_or(framework_terminal);
        let observer = state
            .webhook_observer
            .take()
            .ok_or_else(|| "EKO turn sink was finalized more than once".to_string())?;
        Ok(observer.finish(&receipt, terminal))
    }

    fn record_projector_failure(
        &self,
        code: &str,
        message: impl Into<String>,
    ) -> echo_agent::error::ReactError {
        let message = message.into();
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .downstream_failure = Some(echo_agent::error::AgentFailure::message(
            code,
            message.clone(),
        ));
        echo_agent::error::ReactError::Other(format!("{code}: {message}"))
    }

    #[cfg(test)]
    async fn stop_projector_for_test(&self) -> Result<(), String> {
        let (acknowledgement, receiver) = tokio::sync::oneshot::channel();
        self.sender
            .send(EkoTurnProjectionRequest::Stop { acknowledgement })
            .await
            .map_err(|_| "EKO turn projector was already closed".to_string())?;
        receiver
            .await
            .map_err(|_| "EKO turn projector stop acknowledgement was lost".to_string())
    }
}

#[async_trait::async_trait]
impl EventSink for EkoTurnEventSink {
    async fn on_event(&self, event: EventEnvelope) -> echo_agent::error::Result<SinkControl> {
        let (acknowledgement, receiver) = tokio::sync::oneshot::channel();
        self.sender
            .send(EkoTurnProjectionRequest::Event {
                event: Box::new(event),
                acknowledgement,
            })
            .await
            .map_err(|_| {
                self.record_projector_failure(
                    "sink_projector_closed",
                    "EKO turn projector closed before accepting an envelope",
                )
            })?;
        receiver.await.map_err(|_| {
            self.record_projector_failure(
                "sink_projector_closed",
                "EKO turn projector closed before acknowledging an envelope",
            )
        })?
    }
}

async fn project_eko_turn_event(
    sink: &std::sync::Arc<dyn ChatSink>,
    state: &std::sync::Arc<std::sync::Mutex<EkoTurnEventSinkState>>,
    active_run_id: Option<&str>,
    runtime_store: Option<&std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    turn_id: &str,
    event: EventEnvelope,
) -> echo_agent::error::Result<SinkControl> {
    let reveal_completed = if let (Some(run_id), Some(store)) = (active_run_id, runtime_store) {
        let usage = match &event.payload {
            AgentEvent::LlmUsage {
                prompt_tokens,
                completion_tokens,
                usage_reported: true,
                ..
            } => Some((
                u64::try_from(*prompt_tokens).unwrap_or(u64::MAX),
                u64::try_from(*completion_tokens).unwrap_or(u64::MAX),
            )),
            _ => None,
        };
        let compaction = matches!(&event.payload, AgentEvent::ContextCompressed { .. });
        let reveal = matches!(
            &event.payload,
            AgentEvent::ToolResult { name, .. } if name == "task_execute"
        ) || matches!(&event.payload, AgentEvent::FinalAnswer(_));
        let run_id = run_id.to_string();
        let diagnostic_run_id = run_id.clone();
        let turn_id = turn_id.to_string();
        let event_id = event.event_id.clone();
        match crate::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store.clone())
            .run_store("project EKO turn TaskRuntime event", move |store| {
                if let Some((input_tokens, output_tokens)) = usage
                    && store.account_run_turn_usage(
                        &run_id,
                        &turn_id,
                        event_id.as_str(),
                        input_tokens,
                        output_tokens,
                    )?
                {
                    store.request_pause_with_reason(
                        &run_id,
                        crate::tasks::task_runtime::RunPauseReason::TokenBudget,
                        Some(
                            "the configured token budget was reached at a provider usage boundary",
                        ),
                    )?;
                }
                if compaction {
                    store.record_run_turn_compaction(&run_id, &turn_id, event_id.as_str())?;
                }
                if reveal {
                    return store.get_run(&run_id).map(|run| {
                        run.is_some_and(|run| {
                            run.status == crate::tasks::task_runtime::TaskRunStatus::Completed
                        })
                    });
                }
                Ok(false)
            })
            .await
        {
            Ok(reveal) => reveal,
            Err(error) => {
                tracing::warn!(%error, active_run_id = diagnostic_run_id, "failed to project EKO turn TaskRuntime event");
                false
            }
        }
    } else {
        false
    };

    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.expose_internal_synthesis |= reveal_completed;

    let observer = state.webhook_observer.as_mut().ok_or_else(|| {
        echo_agent::error::ReactError::Other("EKO turn sink was already finalized".to_string())
    })?;
    observer.observe(&event);
    if !state.expose_internal_synthesis
        && matches!(
            &event.payload,
            AgentEvent::ToolResult { name, .. } if name == "task_execute"
        )
    {
        state.expose_internal_synthesis = reveal_completed;
    }
    if !state.expose_internal_synthesis && matches!(&event.payload, AgentEvent::FinalAnswer(_)) {
        state.expose_internal_synthesis = reveal_completed;
    }
    let suppress_internal_transcript = !state.expose_internal_synthesis
        && matches!(
            &event.payload,
            AgentEvent::Token(_)
                | AgentEvent::ThinkStart
                | AgentEvent::ThinkEnd { .. }
                | AgentEvent::FinalAnswer(_)
        );
    if suppress_internal_transcript {
        return Ok(SinkControl::Continue);
    }
    if sink.on_event(ChatDriverEvent::Agent(Box::new(event))) {
        Ok(SinkControl::Continue)
    } else {
        let failure = echo_agent::error::AgentFailure::message(
            "downstream_disconnect",
            "chat event consumer rejected an undelivered framework envelope",
        );
        state.downstream_failure = Some(failure);
        Err(echo_agent::error::ReactError::Other(
            "downstream_disconnect: chat event consumer rejected an undelivered framework envelope"
                .to_string(),
        ))
    }
}

/// Inner ReAct-streaming body of [`drive_chat`], run inside the run_id scope.
async fn drive_chat_inner(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    scope: ChatTurnModelScope,
    input_observer: Option<InputReceiptObserver>,
) -> Result<ChatTurnOutcome, String> {
    let ChatTurnModelScope {
        turn_id,
        bound_run_id,
        driver_execution_context,
        origin,
        transcript_visibility,
    } = scope;
    // The turn preserves user-authored text (or a long-text artifact reference).
    let msg: Message = turn.to_message().map_err(|e| {
        tracing::error!(error = %e, "failed to build user message from prepared turn");
        format!("failed to build user message: {e}")
    })?;
    let cancel = res.cancel.clone();
    let sink: std::sync::Arc<dyn ChatSink> = res.sink.clone();
    let _turn_projection_registration = crate::turn_context::turn_prompt_context_registry()
        .register(turn_id.clone(), origin, turn.authorship);
    let conversation_id = res.conv_id.clone();
    let webhook_emitter = res.webhook_emitter.clone();
    let runtime_store = res.store.clone();
    let active_run_id = bound_run_id;
    // Scope the chat resources into a task_local so tools the agent calls
    // mid-ReAct (create_complex_task / check_run_status / cancel_run, Phase B3)
    // can reach pool/store/sink via `current_chat_resources()`.
    crate::chat_resources::with_chat_resources(res.clone(), async move {
        // The RwLock read guard is held for the stream's lifetime because the
        // stream borrows the agent (same pattern as the GUI's normal chat path).
        let inner = agent.inner().clone();
        let guard = inner.read().await;
        let webhook_observer =
            WebhookTurnObserver::new(webhook_emitter, guard.model_name().to_string());
        let disabled_tools = Some(crate::tool_exposure::disabled_tools());
        let registered_tools = guard.tool_names();
        let visible_tools = if active_run_id.is_some() {
            crate::tool_exposure::initial_visible_tools_for_task_run(&registered_tools)
        } else {
            crate::tool_exposure::initial_visible_tools(&registered_tools)
        };
        crate::tool_exposure::record_schema_budget(&guard.tool_definitions(), &visible_tools);
        let visible_tools = Some(visible_tools);
        let runtime_state_id = guard.conversation_id().map(str::to_string);
        let transcript_generation_id = runtime_state_id
            .as_ref()
            .filter(|runtime_state_id| conversation_id.as_ref() != Some(runtime_state_id))
            .cloned();

        let (working_dir, resource_guards) = res.workspace_io_receipt.as_ref().map_or_else(
            || (Some(res.execution_scope.root().to_path_buf()), Vec::new()),
            |receipt| {
                (
                    Some(receipt.data_root().to_path_buf()),
                    vec![echo_agent::tools::InvocationResourceGuard::new(
                        receipt.clone(),
                    )],
                )
            },
        );

        // `with_run_context` is task-local and does not cross the framework's
        // forked subagent `tokio::spawn`; ExternalRunContext is the value-carried
        // channel that keeps Subagent tools and run_id on this same run. The
        // `trace_sink` here is the framework-Value form; `scoped_with_ctx_run_id`
        // re-scopes it into `CURRENT_TRACE_SINK` for tools (e.g. task_execute)
        // running inside the framework's spawned tool executor.
        let event_identity = EventIdentity::for_chat(
            conversation_id.clone(),
            turn_id.clone(),
            turn_id.clone(),
            active_run_id.clone(),
        )
        .map_err(|error| error.to_string())?;
        let invocation = echo_agent::agent::AgentInvocationContext {
            history: None,
            runtime_state_id,
            transcript_generation_id,
            input_lifecycle: None,
            runtime: Some(echo_agent::tools::ExternalRunContext {
                conversation_id,
                // A real pre-created TaskRun is value-carried across framework
                // spawns. Ordinary task tools derive their prospective
                // scope from turn_id and create it only when invoked.
                run_id: active_run_id.clone(),
                turn_id: Some(turn_id.clone()),
                execution_id: driver_execution_context,
                isolation_id: None,
                message_id: Some(turn_id.clone()),
                cancel: Some(std::sync::Arc::new(cancel.clone())),
                trace_sink: Some(framework_trace_sink_for(&sink)),
                delegation_policy: None,
                resource_guards: Vec::new(),
            }),
            working_dir,
            cancel: None,
            disabled_tools,
            visible_tools,
            run_budget: None,
            resource_guards,
        };
        let eko_sink = EkoTurnEventSink::new(
            sink,
            webhook_observer,
            active_run_id,
            runtime_store,
            turn_id.clone(),
            transcript_visibility,
        );
        let request = TurnRequest::from_message(event_identity, msg)
            .mode(TurnMode::Execute)
            .cancel(cancel)
            .invocation(invocation);
        let (receipt, observer_result) = if let Some(observer) = input_observer {
            let (request, input_receipt) = request.with_input_receipt();
            let driver = AgentTurnDriver.drive(&*guard, request, &eko_sink);
            let (receipt, observer_result) = tokio::join!(driver, observer(input_receipt));
            (receipt, observer_result)
        } else {
            (
                AgentTurnDriver.drive(&*guard, request, &eko_sink).await,
                Ok(()),
            )
        };
        let effective_terminal =
            effective_terminal_after_input_observer(receipt.outcome.clone(), observer_result);
        eko_sink.finish(receipt, effective_terminal)
    })
    .await
}

/// A `ChatSink` that forwards every product event to an mpsc channel.
///
/// Used by surfaces whose renderer consumes a channel and applies a
/// surface-specific presentation after the shared transport boundary.
pub struct ChannelChatSink {
    tx: tokio::sync::mpsc::UnboundedSender<ChatDriverEvent>,
}

impl ChannelChatSink {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<ChatDriverEvent>) -> Self {
        Self { tx }
    }
}

impl ChatSink for ChannelChatSink {
    fn on_event(&self, event: ChatDriverEvent) -> bool {
        // Forward to the channel; if the receiver was dropped (UI quit /
        // channel closed), return false so the driver stops streaming.
        self.tx.send(event).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::TaskRunStatus;

    fn test_execution_scope() -> crate::workspace::WorkspaceExecutionScope {
        crate::workspace::WorkspaceExecutionScope::workspace(
            &crate::workspace::WorkspaceId::from_name("test"),
            ".",
        )
    }

    /// Build a minimal [`PreparedUserTurn`] for tests that do not exercise
    /// spill or attachment logic.
    fn make_turn(text: &str) -> crate::prepared_turn::PreparedUserTurn {
        crate::prepared_turn::PreparedUserTurn {
            instruction: text.to_string(),
            resources: vec![],
            authorship: crate::prepared_turn::InstructionAuthorship::User,
        }
    }

    fn test_turn_receipt(turn_id: &str, outcome: TurnOutcome) -> Result<TurnReceipt, String> {
        let identity = EventIdentity::new(format!("test-stream-{turn_id}"), turn_id)
            .map_err(|error| error.to_string())?;
        Ok(TurnReceipt {
            turn_id: identity.turn_id,
            outcome,
            final_answer: None,
            final_message_id: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            llm_calls: 0,
            compaction_count: 0,
            last_event_sequence: 0,
            elapsed: std::time::Duration::ZERO,
        })
    }

    async fn drive_successful_model_with_observer(
        root_message_id: &str,
        observer_result: Result<(), String>,
    ) -> Result<(ChatTurnOutcome, std::sync::Arc<MockChatSink>), String> {
        let mock = std::sync::Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("observer-terminal")
                .with_response("model completed"),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("observer-terminal")
                .llm_client(mock)
                .build()
                .map_err(|error| error.to_string())?,
        );
        let chat_sink = std::sync::Arc::new(MockChatSink::default());
        let sink: std::sync::Arc<dyn ChatSink> = chat_sink.clone();
        let resources = std::sync::Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: None,
            sink,
            webhook_emitter: None,
            conv_id: Some("observer-conversation".to_string()),
            root_message_id: root_message_id.to_string(),
            attachments: Vec::new(),
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let input_observer: InputReceiptObserver = std::sync::Arc::new(move |_receipt| {
            let result = observer_result.clone();
            Box::pin(async move { result })
        });
        let outcome = drive_chat_turn_with_input_observer(
            &agent,
            &make_turn("complete normally"),
            resources,
            None,
            Some(input_observer),
        )
        .await?;
        Ok((outcome, chat_sink))
    }

    fn prepare_run_turn_for_finalization(
        run_id: &str,
        turn_id: &str,
    ) -> Result<std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>, String> {
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, ExecutionMode, PlanTask, RunTurnOrigin, TaskPlan,
            TaskRunStatus, TurnVisibility,
        };
        let store = std::sync::Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let workspace_id = store.active_workspace_id();
        store
            .create_run(
                run_id,
                &workspace_id,
                "failure-conversation",
                "failure-root",
                DomainProfile::General,
                "survive provider failure",
                "agent_task_plan",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: format!("{run_id}-plan"),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256(
                    "survive provider failure",
                ),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: format!("{run_id}-task"),
                    title: "Continue safely".to_string(),
                    ..PlanTask::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation(run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        if !matches!(
            store
                .claim_run_turn(
                    run_id,
                    turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )
                .map_err(|error| error.to_string())?,
            crate::tasks::task_runtime::store::RunTurnClaimOutcome::Started(_)
        ) {
            return Err("test RunTurn was not claimed".to_string());
        }
        Ok(store)
    }

    #[tokio::test]
    async fn typed_retryable_llm_failure_schedules_without_persisting_provider_message()
    -> Result<(), String> {
        let store = prepare_run_turn_for_finalization("provider-retry", "provider-turn")?;
        let failure = echo_agent::error::AgentFailure {
            category: echo_agent::error::AgentFailureCategory::Llm,
            terminal_kind: echo_agent::error::AgentTerminalKind::Failed,
            retryable: true,
            code: "llm_api".to_string(),
            http_status: Some(503),
            message: "provider secret response body".to_string(),
        };
        let outcome = ChatTurnOutcome::failed("provider-turn".to_string(), failure);
        assert_eq!(
            finalize_run_turn(&store, "provider-retry", &outcome, None).await?,
            RunTurnDecision::Continue
        );
        let snapshot = store
            .get_run_state("provider-retry")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "provider retry snapshot missing".to_string())?;
        assert_eq!(
            snapshot.run.status,
            crate::tasks::task_runtime::TaskRunStatus::Running
        );
        let retry = snapshot
            .continuation
            .and_then(|state| state.provider_retry)
            .ok_or_else(|| "provider retry projection missing".to_string())?;
        assert_eq!(retry.attempt_count, 1);
        let retry_event = store
            .list_events("provider-retry", 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|event| {
                event.event_type
                    == crate::tasks::task_runtime::RuntimeEventKind::RunProviderRetryScheduled
            })
            .ok_or_else(|| "provider retry event missing".to_string())?;
        assert!(!retry_event.payload.to_string().contains("secret response"));
        Ok(())
    }

    #[tokio::test]
    async fn stream_setup_network_failure_preserves_typed_retry_contract() -> Result<(), String> {
        use echo_agent::testing::MockLlmClient;
        use std::sync::Arc;

        let store = prepare_run_turn_for_finalization("setup-retry", "setup-turn")?;
        let mut raw_agent = echo_agent::agent::ReactAgent::new(
            echo_agent::agent::AgentConfig::minimal("setup-retry", "setup-retry")
                .llm_max_retries(0),
        );
        raw_agent.set_llm_client(Arc::new(
            MockLlmClient::new()
                .with_model_name("setup-retry")
                .with_network_error("provider socket unavailable"),
        ));
        let agent = AgentHandle::new(raw_agent);
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("setup-conversation".to_string()),
            root_message_id: "setup-turn".to_string(),
            attachments: Vec::new(),
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let outcome = drive_chat_inner(
            &agent,
            &make_turn("continue"),
            resources,
            ChatTurnModelScope {
                turn_id: "setup-turn".to_string(),
                bound_run_id: Some("setup-retry".to_string()),
                driver_execution_context: None,
                origin: crate::tasks::task_runtime::RunTurnOrigin::Continuation,
                transcript_visibility: crate::tasks::task_runtime::TurnVisibility::Internal,
            },
            None,
        )
        .await?;
        let TurnOutcome::Failed(failure) = &outcome.terminal else {
            return Err(format!("expected typed setup failure, got {outcome:?}"));
        };
        assert_eq!(
            failure.category,
            echo_agent::error::AgentFailureCategory::Llm
        );
        assert!(failure.retryable);
        assert_eq!(
            finalize_run_turn(&store, "setup-retry", &outcome, None).await?,
            RunTurnDecision::Continue
        );
        Ok(())
    }

    #[tokio::test]
    async fn retryable_non_llm_failure_requires_input_instead_of_provider_retry()
    -> Result<(), String> {
        let store = prepare_run_turn_for_finalization("io-failure", "io-turn")?;
        let failure = echo_agent::error::AgentFailure {
            category: echo_agent::error::AgentFailureCategory::Io,
            terminal_kind: echo_agent::error::AgentTerminalKind::Failed,
            retryable: true,
            code: "io".to_string(),
            http_status: None,
            message: "local file unavailable".to_string(),
        };
        let outcome = ChatTurnOutcome::failed("io-turn".to_string(), failure);
        assert_eq!(
            finalize_run_turn(&store, "io-failure", &outcome, None).await?,
            RunTurnDecision::Stop
        );
        let continuation = store
            .get_run_state("io-failure")
            .map_err(|error| error.to_string())?
            .and_then(|snapshot| snapshot.continuation)
            .ok_or_else(|| "IO failure continuation missing".to_string())?;
        assert!(continuation.provider_retry.is_none());
        assert_eq!(
            continuation.pause.map(|pause| pause.reason),
            Some(crate::tasks::task_runtime::RunPauseReason::NeedsInput)
        );
        Ok(())
    }

    #[tokio::test]
    async fn provider_failure_at_token_limit_pauses_as_provider_unavailable() -> Result<(), String>
    {
        let store = prepare_run_turn_for_finalization("provider-budget", "budget-turn")?;
        store
            .update_run_continuation_budgets("provider-budget", Some(1), None)
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .account_run_turn_usage("provider-budget", "budget-turn", "budget-usage", 1, 0,)
                .map_err(|error| error.to_string())?
        );
        let failure = echo_agent::error::AgentFailure {
            category: echo_agent::error::AgentFailureCategory::Llm,
            terminal_kind: echo_agent::error::AgentTerminalKind::Failed,
            retryable: true,
            code: "llm_network".to_string(),
            http_status: None,
            message: "network unavailable".to_string(),
        };
        let outcome = ChatTurnOutcome::failed("budget-turn".to_string(), failure);
        assert_eq!(
            finalize_run_turn(&store, "provider-budget", &outcome, None).await?,
            RunTurnDecision::Stop
        );
        let continuation = store
            .get_run_state("provider-budget")
            .map_err(|error| error.to_string())?
            .and_then(|snapshot| snapshot.continuation)
            .ok_or_else(|| "provider budget continuation missing".to_string())?;
        assert!(
            continuation
                .provider_retry
                .as_ref()
                .is_some_and(|retry| retry.exhausted)
        );
        assert_eq!(
            continuation.pause.map(|pause| pause.reason),
            Some(crate::tasks::task_runtime::RunPauseReason::ProviderUnavailable)
        );
        Ok(())
    }

    struct WorkingDirProbeTool {
        observed: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    }

    impl echo_agent::tools::Tool for WorkingDirProbeTool {
        fn name(&self) -> &str {
            "web_fetch"
        }

        fn description(&self) -> &str {
            "record the invocation working directory"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        fn execute_with_context<'a>(
            &'a self,
            _parameters: echo_agent::tools::ToolParameters,
            context: &'a echo_agent::tools::ToolContext,
        ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<echo_agent::tools::ToolResult>>
        {
            Box::pin(async move {
                *self
                    .observed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = context.working_dir.clone();
                Ok(echo_agent::tools::ToolResult::success("recorded"))
            })
        }
    }

    /// Test-only sink that records received events for assertions.
    struct MockChatSink {
        events: std::sync::Mutex<Vec<EventEnvelope>>,
        execution_paths: std::sync::Mutex<Vec<String>>,
    }

    impl Default for MockChatSink {
        fn default() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
                execution_paths: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl ChatSink for MockChatSink {
        fn on_event(&self, event: ChatDriverEvent) -> bool {
            match event {
                ChatDriverEvent::Agent(event) => self
                    .events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(*event),
                ChatDriverEvent::ExecutionPath { observed_path } => self
                    .execution_paths
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(observed_path),
                ChatDriverEvent::Execution(_)
                | ChatDriverEvent::TurnStatus { .. }
                | ChatDriverEvent::TurnConfiguration { .. }
                | ChatDriverEvent::Interrupt { .. }
                | ChatDriverEvent::InputLifecycle(_)
                | ChatDriverEvent::ApprovalRequest { .. }
                | ChatDriverEvent::InputRequest { .. }
                | ChatDriverEvent::SelectionRequest { .. }
                | ChatDriverEvent::ExtensionReceipt(_)
                | ChatDriverEvent::CommandCellStarted { .. }
                | ChatDriverEvent::CommandCellSettled { .. }
                | ChatDriverEvent::AwaiterResultReady { .. }
                | ChatDriverEvent::AwaiterResultDeliveryStarted { .. }
                | ChatDriverEvent::AwaiterResultAcknowledged { .. }
                | ChatDriverEvent::ContextCompressed { .. } => {}
            }
            true
        }
    }

    impl MockChatSink {
        fn has_final_answer(&self) -> bool {
            self.events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|e| matches!(e.payload, AgentEvent::FinalAnswer(_)))
        }
        fn event_count(&self) -> usize {
            self.events.lock().unwrap_or_else(|e| e.into_inner()).len()
        }
        fn has_run_identity(&self) -> bool {
            self.events
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .any(|event| event.run_id.is_some())
        }
        fn has_valid_contract(&self, conversation_id: &str, turn_id: &str) -> bool {
            let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
            let terminal_count = events
                .iter()
                .filter(|event| event.payload.is_terminal())
                .count();
            events.iter().enumerate().all(|(index, event)| {
                event.schema_version == echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION
                    && event.sequence == (index as u64).saturating_add(1)
                    && event.conversation_id.as_ref().map(|id| id.as_str()) == Some(conversation_id)
                    && event.turn_id.as_str() == turn_id
                    && event.run_id.is_none()
                    && !event.event_id.as_str().is_empty()
            }) && terminal_count == 1
        }
    }

    #[tokio::test]
    async fn eko_turn_sink_preserves_product_receipt_fields() -> Result<(), String> {
        let sink = std::sync::Arc::new(MockChatSink::default());
        let store = std::sync::Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let workspace_id = store.active_workspace_id();
        store
            .create_run(
                "receipt-run",
                &workspace_id,
                "receipt-conversation",
                "receipt-message",
                crate::tasks::task_runtime::DomainProfile::General,
                "account reported usage",
                "test",
                crate::tasks::task_runtime::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("receipt-run", true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("receipt-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .claim_run_turn(
                "receipt-run",
                "receipt-turn",
                RunTurnOrigin::User,
                TurnVisibility::Visible,
            )
            .map_err(|error| error.to_string())?;
        let adapter = EkoTurnEventSink::new(
            sink.clone(),
            WebhookTurnObserver::new(None, "test-model".to_string()),
            Some("receipt-run".to_string()),
            Some(store.clone()),
            "receipt-turn".to_string(),
            TurnVisibility::Visible,
        );
        let identity = EventIdentity::for_chat(
            Some("receipt-conversation".to_string()),
            "receipt-turn",
            "receipt-turn",
            Some("receipt-run".to_string()),
        )
        .map_err(|error| error.to_string())?;
        let events = [
            AgentEvent::LlmUsage {
                model: "unreported".to_string(),
                prompt_tokens: 100,
                completion_tokens: 200,
                total_tokens: 300,
                cached_prompt_tokens: 0,
                cache_creation_prompt_tokens: 0,
                usage_reported: false,
            },
            AgentEvent::LlmUsage {
                model: "test-model".to_string(),
                prompt_tokens: 11,
                completion_tokens: 7,
                total_tokens: 18,
                cached_prompt_tokens: 0,
                cache_creation_prompt_tokens: 0,
                usage_reported: true,
            },
            AgentEvent::ContextCompressed {
                before_count: 10,
                after_count: 4,
                before_tokens: 1_000,
                after_tokens: 400,
            },
            AgentEvent::FinalAnswer("finished".to_string()),
        ];
        for (index, event) in events.into_iter().enumerate() {
            let sequence = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
            let envelope = EventEnvelope::new(&identity, sequence, None, event)
                .map_err(|error| error.to_string())?;
            assert_eq!(
                adapter
                    .on_event(envelope)
                    .await
                    .map_err(|error| error.to_string())?,
                SinkControl::Continue
            );
        }

        let mut receipt = test_turn_receipt("receipt-turn", TurnOutcome::Completed)?;
        receipt.final_answer = Some("finished".to_string());
        receipt.final_message_id = EventIdentity::for_chat(
            Some("receipt-conversation".to_string()),
            "receipt-turn",
            "receipt-turn",
            Some("receipt-run".to_string()),
        )
        .map_err(|error| error.to_string())?
        .message_id;
        receipt.prompt_tokens = 11;
        receipt.completion_tokens = 7;
        receipt.llm_calls = 1;
        receipt.compaction_count = 1;
        let outcome = adapter.finish(receipt, TurnOutcome::Completed)?;
        assert_eq!(outcome.terminal, TurnOutcome::Completed);
        assert_eq!(outcome.input_tokens, 11);
        assert_eq!(outcome.output_tokens, 7);
        assert_eq!(outcome.compaction_count, 1);
        assert_eq!(outcome.final_answer.as_deref(), Some("finished"));
        assert_eq!(outcome.final_message_id.as_deref(), Some("receipt-turn"));
        assert_eq!(sink.event_count(), 4);
        let continuation = store
            .get_run_state("receipt-run")
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "receipt continuation missing".to_string())?;
        assert_eq!(continuation.tokens_used, 18);
        Ok(())
    }

    struct RejectingChatSink;

    impl ChatSink for RejectingChatSink {
        fn on_event(&self, _event: ChatDriverEvent) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn rejected_envelope_is_a_typed_downstream_failure() -> Result<(), String> {
        let adapter = EkoTurnEventSink::new(
            std::sync::Arc::new(RejectingChatSink),
            WebhookTurnObserver::new(None, "test-model".to_string()),
            None,
            None,
            "rejected-turn".to_string(),
            TurnVisibility::Visible,
        );
        let identity = EventIdentity::new("rejected-stream", "rejected-turn")
            .map_err(|error| error.to_string())?;
        let envelope = EventEnvelope::new(
            &identity,
            1,
            None,
            AgentEvent::Token("undelivered".to_string()),
        )
        .map_err(|error| error.to_string())?;
        let error = match adapter.on_event(envelope).await {
            Err(error) => error,
            Ok(control) => {
                return Err(format!("rejected envelope was accepted with {control:?}"));
            }
        };
        assert!(error.to_string().contains("downstream_disconnect"));

        let receipt = test_turn_receipt("rejected-turn", TurnOutcome::Cancelled)?;
        let outcome = adapter.finish(receipt, TurnOutcome::Cancelled)?;
        assert!(matches!(
            outcome.terminal,
            TurnOutcome::Failed(ref failure) if failure.code == "downstream_disconnect"
        ));
        Ok(())
    }

    #[derive(Default)]
    struct SlowOrderedChatSink {
        sequences: std::sync::Mutex<Vec<u64>>,
    }

    impl ChatSink for SlowOrderedChatSink {
        fn on_event(&self, event: ChatDriverEvent) -> bool {
            let ChatDriverEvent::Agent(event) = event else {
                return true;
            };
            std::thread::sleep(std::time::Duration::from_millis(2));
            self.sequences
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(event.sequence);
            true
        }
    }

    #[tokio::test]
    async fn bounded_projector_preserves_burst_order_and_ack_backpressure() -> Result<(), String> {
        const EVENTS: u64 = 96;
        let sink = std::sync::Arc::new(SlowOrderedChatSink::default());
        let adapter = EkoTurnEventSink::new(
            sink.clone(),
            WebhookTurnObserver::new(None, "test-model".to_string()),
            None,
            None,
            "burst-turn".to_string(),
            TurnVisibility::Visible,
        );
        let identity =
            EventIdentity::new("burst-stream", "burst-turn").map_err(|error| error.to_string())?;
        let mut deliveries = Vec::new();
        for sequence in 1..=EVENTS {
            let envelope = EventEnvelope::new(
                &identity,
                sequence,
                None,
                AgentEvent::Token(format!("token-{sequence}")),
            )
            .map_err(|error| error.to_string())?;
            deliveries.push(adapter.on_event(envelope));
        }
        let results = futures::future::join_all(deliveries).await;
        assert!(
            results
                .into_iter()
                .all(|result| matches!(result, Ok(SinkControl::Continue)))
        );
        assert_eq!(
            *sink
                .sequences
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            (1..=EVENTS).collect::<Vec<_>>()
        );
        Ok(())
    }

    #[tokio::test]
    async fn closed_projector_returns_typed_sink_error() -> Result<(), String> {
        let adapter = EkoTurnEventSink::new(
            std::sync::Arc::new(MockChatSink::default()),
            WebhookTurnObserver::new(None, "test-model".to_string()),
            None,
            None,
            "closed-projector-turn".to_string(),
            TurnVisibility::Visible,
        );
        adapter.stop_projector_for_test().await?;
        let identity = EventIdentity::new("closed-projector-stream", "closed-projector-turn")
            .map_err(|error| error.to_string())?;
        let envelope = EventEnvelope::new(
            &identity,
            1,
            None,
            AgentEvent::Token("undelivered".to_string()),
        )
        .map_err(|error| error.to_string())?;
        let error = match adapter.on_event(envelope).await {
            Err(error) => error,
            Ok(control) => {
                return Err(format!(
                    "closed projector accepted delivery with {control:?}"
                ));
            }
        };
        assert!(error.to_string().contains("sink_projector_closed"));
        let receipt = test_turn_receipt("closed-projector-turn", TurnOutcome::Cancelled)?;
        let outcome = adapter.finish(receipt, TurnOutcome::Cancelled)?;
        assert!(matches!(
            outcome.terminal,
            TurnOutcome::Failed(ref failure) if failure.code == "sink_projector_closed"
        ));
        Ok(())
    }

    async fn wait_for_run_status(
        store: &crate::tasks::task_runtime::TaskRuntimeStore,
        run_id: &str,
        expected: crate::tasks::task_runtime::TaskRunStatus,
    ) -> Result<crate::tasks::task_runtime::TaskRun, String> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(run) = store.get_run(run_id).map_err(|error| error.to_string())?
                    && run.status == expected
                {
                    return Ok(run);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            let current_status = store
                .get_run(run_id)
                .ok()
                .flatten()
                .map(|run| run.status.as_str().to_string())
                .unwrap_or_else(|| "missing".to_string());
            let event_types = store
                .list_events(run_id, 0)
                .map(|events| {
                    events
                        .into_iter()
                        .map(|event| {
                            format!(
                                "{:?}[status={:?},turn={:?},reason={:?},kind={:?}]",
                                event.event_type,
                                event.payload.get("status"),
                                event.payload.get("turn_id"),
                                event.payload.get("reason"),
                                event.payload.get("kind"),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_else(|error| format!("unavailable:{error}"));
            let continuation_state =
                crate::tasks::task_runtime::continuation::runtime_state_for_test(store, run_id);
            format!(
                "timed out waiting for {run_id} to become {}; current={current_status}; continuation={continuation_state:?}; events={event_types}",
                expected.as_str(),
            )
        })?
    }

    #[test]
    fn subsecond_turn_time_is_rounded_up_for_budget_accounting() {
        assert_eq!(duration_seconds_rounded_up(std::time::Duration::ZERO), 0);
        assert_eq!(
            duration_seconds_rounded_up(std::time::Duration::from_millis(1)),
            1
        );
        assert_eq!(
            duration_seconds_rounded_up(std::time::Duration::from_millis(1_001)),
            2
        );
    }

    #[test]
    fn pinned_memory_generation_precedes_live_rebind_admission() -> Result<(), String> {
        use echo_agent::evolution::ReviewConfig;
        use echo_agent::memory::{InMemoryStore, Store};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let pinned_root = temp.path().join("pinned/.eko");
        let pinned_integration = crate::evolution::ReviewIntegration::new(
            ReviewConfig::default(),
            pinned_root.clone(),
            std::sync::Arc::new(InMemoryStore::new()) as std::sync::Arc<dyn Store>,
        );
        let pinned = pinned_integration
            .lease_generation()
            .map_err(|error| error.to_string())?;

        let live_integration = std::sync::Arc::new(crate::evolution::ReviewIntegration::new(
            ReviewConfig::default(),
            temp.path().join("live/.eko"),
            std::sync::Arc::new(InMemoryStore::new()) as std::sync::Arc<dyn Store>,
        ));
        let _rebind = live_integration
            .prepare_rebind(
                temp.path().join("next/.eko"),
                std::sync::Arc::new(InMemoryStore::new()) as std::sync::Arc<dyn Store>,
            )
            .map_err(|error| error.to_string())?;
        let resources = crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: None,
            sink: std::sync::Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("pinned-memory-conversation".to_string()),
            root_message_id: "pinned-memory-turn".to_string(),
            attachments: Vec::new(),
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: Some(live_integration),
            layer_manager: None,
            memory_generation: Some(pinned),
            human_loop_provider: None,
        };

        let resolved = resolve_turn_memory_generation(&resources)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "pinned memory generation was discarded".to_string())?;
        assert_eq!(resolved.echo_agent_dir(), pinned_root);
        Ok(())
    }

    #[tokio::test]
    async fn pooled_chat_rejects_closed_task_runtime_before_pool_admission() -> Result<(), String> {
        use echo_agent::agent::ReactAgentBuilder;
        use echo_agent::testing::MockLlmClient;
        use std::sync::Arc;

        let agent = AgentHandle::new(
            ReactAgentBuilder::new()
                .model("pooled-order")
                .llm_client(Arc::new(
                    MockLlmClient::new().with_model_name("pooled-order"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let pool =
            Arc::new(crate::agent_pool::AgentPool::new_for_test(agent, None, None, 3, false).await);
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;

        let configured = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let configured_for_call = configured.clone();
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: Some(pool.clone()),
            store: Some(store),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("pooled-order-conversation".to_string()),
            root_message_id: "pooled-order-turn".to_string(),
            attachments: Vec::new(),
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let result = drive_pooled_chat(
            pool.clone(),
            "pooled-order-conversation",
            move |_agent| async move {
                configured_for_call.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
            &make_turn("do not enter the pool"),
            resources,
        )
        .await;

        let error = match result {
            Ok(outcome) => return Err(format!("closed TaskRuntime admitted {outcome:?}")),
            Err(error) => error,
        };
        assert!(error.contains("admission"));
        assert!(!configured.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_during_agent_configuration_terminalizes_the_claimed_run()
    -> Result<(), String> {
        use echo_agent::agent::{CancellationToken, ReactAgentBuilder};
        use echo_agent::testing::MockLlmClient;
        use std::sync::Arc;

        let agent = AgentHandle::new(
            ReactAgentBuilder::new()
                .model("pre-driver-cancel")
                .llm_client(Arc::new(
                    MockLlmClient::new().with_model_name("pre-driver-cancel"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let pool =
            Arc::new(crate::agent_pool::AgentPool::new_for_test(agent, None, None, 3, false).await);
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "pre-driver-cancel-run",
                "test",
                "pre-driver-cancel-conversation",
                "pre-driver-cancel-root",
                crate::tasks::task_runtime::DomainProfile::General,
                "cancel this exact run",
                "agent_task_plan",
                crate::tasks::task_runtime::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(
                "pre-driver-cancel-run",
                crate::tasks::task_runtime::TaskRunStatus::Running,
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("pre-driver-cancel-run", true, false, None, None)
            .map_err(|error| error.to_string())?;
        let cancel = CancellationToken::new();
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: Some(pool.clone()),
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("pre-driver-cancel-conversation".to_string()),
            root_message_id: "pre-driver-cancel-turn".to_string(),
            attachments: Vec::new(),
            cancel,
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let (configured_tx, configured_rx) = tokio::sync::oneshot::channel();
        let turn = make_turn("wait in configuration");
        let task = tokio::spawn({
            let pool = pool.clone();
            let resources = resources.clone();
            async move {
                drive_pooled_chat_turn(
                    pool,
                    "__continuation__:pre-driver-cancel-run",
                    move |_| async move {
                        let _delivered = configured_tx.send(());
                        std::future::pending::<Result<(), String>>().await
                    },
                    &turn,
                    resources,
                    RunTurnBinding {
                        run_id: Some("pre-driver-cancel-run".to_string()),
                        turn_id: "pre-driver-cancel-turn".to_string(),
                        root_message_id: "pre-driver-cancel-root".to_string(),
                        origin: RunTurnOrigin::User,
                        transcript_visibility: TurnVisibility::Visible,
                        expected_resume: None,
                    },
                )
                .await
            }
        });
        configured_rx
            .await
            .map_err(|_| "configuration barrier closed".to_string())?;
        if !store
            .request_cancel("pre-driver-cancel-run")
            .map_err(|error| error.to_string())?
        {
            return Err("active pre-driver run was not cancelled".to_string());
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .map_err(|_| "cancelled configuration did not settle".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(result.is_err());
        store
            .wait_for_run_driver_idle("pre-driver-cancel-run")
            .await;
        let snapshot = store
            .get_run_state("pre-driver-cancel-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "cancelled run snapshot missing".to_string())?;
        assert_eq!(
            snapshot.run.status,
            crate::tasks::task_runtime::TaskRunStatus::Cancelled
        );
        assert!(
            snapshot
                .continuation
                .is_some_and(|continuation| continuation.active_turn.is_none())
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_during_agent_configuration_preserves_boot_recovery() -> Result<(), String> {
        use echo_agent::agent::{CancellationToken, ReactAgentBuilder};
        use echo_agent::testing::MockLlmClient;
        use std::sync::Arc;

        let agent = AgentHandle::new(
            ReactAgentBuilder::new()
                .model("pre-driver-shutdown")
                .llm_client(Arc::new(
                    MockLlmClient::new().with_model_name("pre-driver-shutdown"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let pool =
            Arc::new(crate::agent_pool::AgentPool::new_for_test(agent, None, None, 3, false).await);
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "pre-driver-shutdown-run",
                "test",
                "pre-driver-shutdown-conversation",
                "pre-driver-shutdown-root",
                crate::tasks::task_runtime::DomainProfile::General,
                "recover after shutdown",
                "agent_task_plan",
                crate::tasks::task_runtime::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(
                "pre-driver-shutdown-run",
                crate::tasks::task_runtime::TaskRunStatus::Running,
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("pre-driver-shutdown-run", true, false, None, None)
            .map_err(|error| error.to_string())?;
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: Some(pool.clone()),
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("pre-driver-shutdown-conversation".to_string()),
            root_message_id: "pre-driver-shutdown-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let (configured_tx, configured_rx) = tokio::sync::oneshot::channel();
        let turn = make_turn("wait in configuration");
        let task = tokio::spawn({
            let pool = pool.clone();
            let resources = resources.clone();
            async move {
                drive_pooled_chat_turn(
                    pool,
                    "__continuation__:pre-driver-shutdown-run",
                    move |_| async move {
                        let _delivered = configured_tx.send(());
                        std::future::pending::<Result<(), String>>().await
                    },
                    &turn,
                    resources,
                    RunTurnBinding {
                        run_id: Some("pre-driver-shutdown-run".to_string()),
                        turn_id: "pre-driver-shutdown-turn".to_string(),
                        root_message_id: "pre-driver-shutdown-root".to_string(),
                        origin: RunTurnOrigin::User,
                        transcript_visibility: TurnVisibility::Visible,
                        expected_resume: None,
                    },
                )
                .await
            }
        });
        configured_rx
            .await
            .map_err(|_| "configuration barrier closed".to_string())?;
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .map_err(|_| "shutdown configuration did not settle".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(result.is_err());
        let snapshot = store
            .get_run_state("pre-driver-shutdown-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "shutdown run snapshot missing".to_string())?;
        assert_eq!(
            snapshot.run.status,
            crate::tasks::task_runtime::TaskRunStatus::Paused
        );
        assert_eq!(
            snapshot
                .continuation
                .and_then(|continuation| continuation.pause)
                .map(|pause| pause.reason),
            Some(crate::tasks::task_runtime::RunPauseReason::BootRecovery)
        );
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_run_turn_loser_does_not_fail_the_authoritative_winner() -> Result<(), String>
    {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;

        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("claim-race")
                .with_delay(std::time::Duration::from_millis(150))
                .with_responses(["one", "two", "three"]),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("claim-race")
                .llm_client(llm)
                .build()
                .map_err(|error| error.to_string())?,
        );
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "claim-race-run",
                "test",
                "claim-race-conversation",
                "claim-race-root",
                crate::tasks::task_runtime::DomainProfile::General,
                "preserve the winner",
                "agent_task_plan",
                crate::tasks::task_runtime::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(
                "claim-race-run",
                crate::tasks::task_runtime::TaskRunStatus::Running,
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("claim-race-run", true, false, None, None)
            .map_err(|error| error.to_string())?;

        let resources_for = |turn_id: &str| {
            Arc::new(crate::chat_resources::ChatResources {
                execution_scope: test_execution_scope(),
                workspace_io_receipt: None,
                pool: None,
                store: Some(store.clone()),
                sink: Arc::new(MockChatSink::default()),
                webhook_emitter: None,
                conv_id: Some("claim-race-conversation".to_string()),
                root_message_id: turn_id.to_string(),
                attachments: Vec::new(),
                cancel: CancellationToken::new(),
                review_integration: None,
                layer_manager: None,
                memory_generation: None,
                human_loop_provider: None,
            })
        };
        let winner_agent = agent.clone();
        let winner_resources = resources_for("winner-turn");
        let winner = tokio::spawn(async move {
            drive_chat_turn(
                &winner_agent,
                &make_turn("winner"),
                winner_resources,
                Some(RunTurnBinding {
                    run_id: Some("claim-race-run".to_string()),
                    turn_id: "winner-turn".to_string(),
                    root_message_id: "claim-race-root".to_string(),
                    origin: RunTurnOrigin::User,
                    transcript_visibility: TurnVisibility::Visible,
                    expected_resume: None,
                }),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if store
                    .get_run_state("claim-race-run")
                    .ok()
                    .flatten()
                    .and_then(|state| state.continuation)
                    .is_some_and(|continuation| continuation.active_turn.is_some())
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| "winner did not claim its RunTurn".to_string())?;

        let loser = drive_chat_turn(
            &agent,
            &make_turn("loser"),
            resources_for("loser-turn"),
            Some(RunTurnBinding {
                run_id: Some("claim-race-run".to_string()),
                turn_id: "loser-turn".to_string(),
                root_message_id: "claim-race-root".to_string(),
                origin: RunTurnOrigin::Continuation,
                transcript_visibility: TurnVisibility::Internal,
                expected_resume: None,
            }),
        )
        .await;
        assert!(loser.is_err());
        assert_ne!(
            store
                .get_run("claim-race-run")
                .map_err(|error| error.to_string())?
                .map(|run| run.status),
            Some(crate::tasks::task_runtime::TaskRunStatus::Failed)
        );
        winner
            .await
            .map_err(|error| format!("winner task failed: {error}"))??;
        wait_for_run_status(
            &store,
            "claim-race-run",
            crate::tasks::task_runtime::TaskRunStatus::Paused,
        )
        .await?;
        Ok(())
    }

    struct TaskExecuteReceiptBarrierSink {
        reached: std::sync::mpsc::SyncSender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        parked: std::sync::atomic::AtomicBool,
    }

    impl ChatSink for TaskExecuteReceiptBarrierSink {
        fn on_event(&self, event: ChatDriverEvent) -> bool {
            let should_park = matches!(
                event,
                ChatDriverEvent::Agent(ref envelope)
                    if matches!(
                        &envelope.payload,
                        AgentEvent::ToolResult { name, .. } if name == "task_execute"
                    )
            ) && !self.parked.swap(true, std::sync::atomic::Ordering::SeqCst);
            if !should_park {
                return true;
            }
            if self.reached.send(()).is_err() {
                return false;
            }
            self.release
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok()
        }
    }

    #[tokio::test]
    async fn explicit_resume_binding_keeps_one_goal_across_new_turn_ids() -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;

        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("resume-test")
                .with_responses(["resume pass one", "resume pass two", "resume pass three"]),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("resume-test")
                .llm_client(llm)
                .build()
                .map_err(|error| error.to_string())?,
        );
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "existing-goal",
                "test",
                "resume-conversation",
                "root-message",
                crate::tasks::task_runtime::DomainProfile::General,
                "preserve this exact goal",
                "agent_task_plan",
                crate::tasks::task_runtime::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&crate::tasks::task_runtime::TaskPlan {
                plan_id: "existing-goal-plan".to_string(),
                run_id: "existing-goal".to_string(),
                revision: 1,
                domain_profile: crate::tasks::task_runtime::DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256(
                    "preserve this exact goal",
                ),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: crate::tasks::task_runtime::ExecutionMode::Sequential,
                tasks: vec![crate::tasks::task_runtime::PlanTask {
                    id: "existing-goal-task".to_string(),
                    title: "Continue the goal".to_string(),
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run(
                "existing-goal",
                crate::tasks::task_runtime::TaskRunStatus::Running,
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("existing-goal", true, false, None, None)
            .map_err(|error| error.to_string())?;
        if !store
            .request_pause("existing-goal")
            .map_err(|error| error.to_string())?
        {
            return Err("idle long-horizon run was not paused".to_string());
        }
        let expected_resume = crate::tasks::task_runtime::TaskRunResumeIdentity::capture(
            &store
                .get_run_state("existing-goal")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "paused TaskRun state disappeared".to_string())?,
        );
        store
            .record_execution_path("existing-goal", "formal_plan")
            .map_err(|error| error.to_string())?;
        let sink = Arc::new(MockChatSink::default());
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink,
            webhook_emitter: None,
            conv_id: Some("resume-conversation".to_string()),
            root_message_id: "surface-resume-message".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        drive_chat_turn(
            &agent,
            &make_turn("continue the existing goal"),
            resources,
            Some(RunTurnBinding::resume_expected(
                expected_resume,
                "resume-turn",
            )),
        )
        .await?;

        let run = wait_for_run_status(
            &store,
            "existing-goal",
            crate::tasks::task_runtime::TaskRunStatus::Paused,
        )
        .await?;
        assert_eq!(run.goal, "preserve this exact goal");
        assert_eq!(run.root_message_id, "root-message");
        assert_eq!(
            run.status,
            crate::tasks::task_runtime::TaskRunStatus::Paused
        );
        let starts = store
            .list_events("existing-goal", 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| event.event_type == RuntimeEventKind::RunTurnStarted)
            .collect::<Vec<_>>();
        assert_eq!(starts.len(), 3);
        assert_eq!(
            starts
                .first()
                .and_then(|event| event.payload.get("turn_id"))
                .and_then(serde_json::Value::as_str),
            Some("resume-turn")
        );
        assert_eq!(
            starts
                .first()
                .and_then(|event| event.payload.get("origin"))
                .and_then(serde_json::Value::as_str),
            Some("resume")
        );
        let derived = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(
            "surface-resume-message",
        );
        assert!(
            store
                .get_run(&derived)
                .map_err(|error| error.to_string())?
                .is_none()
        );

        let ordinary_agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("ordinary-chat-test")
                .llm_client(Arc::new(
                    echo_agent::testing::MockLlmClient::new()
                        .with_model_name("ordinary-chat-test")
                        .with_response("ordinary chat remains independent"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        drive_chat(
            &ordinary_agent,
            &make_turn("continue"),
            Arc::new(crate::chat_resources::ChatResources {
                execution_scope: test_execution_scope(),
                workspace_io_receipt: None,
                pool: None,
                store: Some(store.clone()),
                sink: Arc::new(MockChatSink::default()),
                webhook_emitter: None,
                conv_id: Some("resume-conversation".to_string()),
                root_message_id: "auto-resume-message".to_string(),
                attachments: Vec::new(),
                cancel: CancellationToken::new(),
                review_integration: None,
                layer_manager: None,
                memory_generation: None,
                human_loop_provider: None,
            }),
        )
        .await?;
        let unchanged_starts = store
            .list_events("existing-goal", 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| event.event_type == RuntimeEventKind::RunTurnStarted)
            .collect::<Vec<_>>();
        assert_eq!(unchanged_starts.len(), 3);
        let auto_derived =
            crate::tasks::task_runtime::task_tools::formal_run_id_for_turn("auto-resume-message");
        assert!(
            store
                .get_run(&auto_derived)
                .map_err(|error| error.to_string())?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn stale_expected_resume_binding_changes_no_run_state_or_attachments()
    -> Result<(), String> {
        use std::sync::Arc;

        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "stale-resume",
                "test",
                "stale-conversation",
                "stale-root",
                crate::tasks::task_runtime::DomainProfile::General,
                "preserve replacement",
                "agent_task_plan",
                crate::tasks::task_runtime::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&crate::tasks::task_runtime::TaskPlan {
                plan_id: "stale-plan".to_string(),
                run_id: "stale-resume".to_string(),
                revision: 1,
                domain_profile: crate::tasks::task_runtime::DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256("preserve replacement"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: crate::tasks::task_runtime::ExecutionMode::Sequential,
                tasks: vec![crate::tasks::task_runtime::PlanTask {
                    id: "stale-task".to_string(),
                    title: "Keep state".to_string(),
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run(
                "stale-resume",
                crate::tasks::task_runtime::TaskRunStatus::Running,
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("stale-resume", true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .request_pause("stale-resume")
            .map_err(|error| error.to_string())?;
        let before = store
            .get_run_state("stale-resume")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stale resume state missing".to_string())?;
        let before_events = store
            .list_events("stale-resume", 0)
            .map_err(|error| error.to_string())?
            .len();
        let expected = crate::tasks::task_runtime::TaskRunResumeIdentity::capture(&before);
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("stale-conversation".to_string()),
            root_message_id: "new-surface-turn".to_string(),
            attachments: vec![crate::attachments::AttachmentRef {
                path: std::path::PathBuf::from("/tmp/stale-resume-attachment"),
                name: "must-not-persist.txt".to_string(),
                mime_type: "text/plain".to_string(),
                source: crate::types::AttachmentSource::default(),
            }],
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let mut conflicting_binding =
            RunTurnBinding::resume_expected(expected.clone(), "conflicting-turn");
        conflicting_binding.run_id = Some("different-run".to_string());
        let conflict = prepare_chat_execution(
            &make_turn("resume replacement"),
            resources.clone(),
            Some(conflicting_binding),
        )
        .await;
        let conflict_error = match conflict {
            Ok(_) => return Err("conflicting expected resume unexpectedly prepared".to_string()),
            Err(error) => error,
        };
        assert!(conflict_error.contains("does not match its execution scope"));
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        assert_eq!(
            store
                .list_events("stale-resume", 0)
                .map_err(|error| error.to_string())?
                .len(),
            before_events
        );

        let mut stale_expected = expected;
        stale_expected.goal_revision = stale_expected.goal_revision.saturating_add(1);
        let result = prepare_chat_execution(
            &make_turn("resume replacement"),
            resources,
            Some(RunTurnBinding::resume_expected(
                stale_expected,
                "stale-turn",
            )),
        )
        .await;
        let error = match result {
            Ok(_) => return Err("stale expected resume unexpectedly prepared".to_string()),
            Err(error) => error,
        };
        assert!(error.contains("identity changed"));
        let after = store
            .get_run_state("stale-resume")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stale resume state disappeared".to_string())?;
        assert_eq!(
            after.run.status,
            crate::tasks::task_runtime::TaskRunStatus::Paused
        );
        assert!(after.run.attachments.is_empty());
        assert_eq!(after.continuation, before.continuation);
        assert_eq!(
            store
                .list_events("stale-resume", 0)
                .map_err(|error| error.to_string())?
                .len(),
            before_events
        );
        Ok(())
    }

    async fn drive_scripted_task_graph(turn_id: &str) -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use echo_agent::agent::subagent::SubagentDefinition;
        use std::sync::Arc;

        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("task-path")
                .then_tool_call(
                    "create-plan",
                    "task_create",
                    serde_json::json!({
                        "tasks": [{
                            "id": "inspect",
                            "title": "Inspect runtime",
                            "description": "Inspect the runtime and report evidence",
                            "kind": "read_only_review",
                            "subagent": "explorer"
                        }]
                    })
                    .to_string(),
                )
                .then_tool_call("execute-plan", "task_execute", r#"{"revision":1}"#)
                .with_response("task graph finished"),
        );
        let mut react_agent = echo_agent::agent::ReactAgentBuilder::new()
            .model("task-path")
            .llm_client(llm.clone())
            .build()
            .map_err(|error| error.to_string())?;
        react_agent.register_subagent_with_definition(
            SubagentDefinition::new("explorer", "Inspect runtime evidence"),
            Box::new(echo_agent::testing::MockAgent::new("explorer").with_response("inspected")),
        );
        let agent = AgentHandle::new(react_agent);
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        crate::tasks::task_runtime::register_task_tools_on_agent(&agent, store.clone()).await;
        let sink: Arc<dyn ChatSink> = Arc::new(MockChatSink::default());
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink,
            webhook_emitter: None,
            conv_id: Some(format!("{turn_id}-conversation")),
            root_message_id: turn_id.to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let outcome = drive_chat(&agent, &make_turn("run the task graph"), resources).await?;
        if outcome != TurnOutcome::Completed {
            return Err(format!("task graph turn ended {outcome:?}"));
        }
        let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(turn_id);
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                let observed_runs = store
                    .list_runs_in(&[
                        crate::tasks::task_runtime::TaskRunStatus::Pending,
                        crate::tasks::task_runtime::TaskRunStatus::Running,
                        crate::tasks::task_runtime::TaskRunStatus::Paused,
                        crate::tasks::task_runtime::TaskRunStatus::Cancelled,
                        crate::tasks::task_runtime::TaskRunStatus::Failed,
                        crate::tasks::task_runtime::TaskRunStatus::Completed,
                    ])
                    .map(|runs| {
                        runs.into_iter()
                            .map(|run| run.run_id)
                            .collect::<Vec<_>>()
                    });
                format!(
                    "task_create did not create TaskRun {run_id} after {} LLM calls; observed runs: {observed_runs:?}; last request: {:?}",
                    llm.call_count(),
                    llm.last_messages()
                )
            })?;
        assert_eq!(run.run_id, run_id);
        if !matches!(
            run.status,
            crate::tasks::task_runtime::TaskRunStatus::Completed
                | crate::tasks::task_runtime::TaskRunStatus::Failed
                | crate::tasks::task_runtime::TaskRunStatus::Paused
                | crate::tasks::task_runtime::TaskRunStatus::Cancelled
        ) {
            return Err(format!("task_execute left run {:?}", run.status));
        }
        let plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "task_create did not persist a plan".to_string())?;
        assert_eq!(plan.run_id, run_id);
        assert_eq!(plan.tasks.len(), 1);
        let task_status = plan.tasks.first().map(|task| task.status.clone());
        assert_ne!(
            task_status,
            Some(echo_agent::tasks::TaskStatus::Pending),
            "task_execute must advance the task created under outer run {run_id}"
        );
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(store.active_run_driver_count()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn task_create_and_execute_use_the_canonical_turn_driver() -> Result<(), String> {
        drive_scripted_task_graph("task-graph").await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawned_task_execute_retains_pool_receipt_until_outer_driver_settles()
    -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use echo_agent::agent::subagent::SubagentDefinition;
        use std::sync::Arc;

        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("task-pool-path")
                .then_tool_call(
                    "create-plan",
                    "task_create",
                    serde_json::json!({
                        "tasks": [{
                            "id": "inspect",
                            "title": "Inspect pooled runtime",
                            "description": "Inspect the pooled runtime",
                            "kind": "read_only_review",
                            "subagent": "explorer"
                        }]
                    })
                    .to_string(),
                )
                .then_tool_call("execute-plan", "task_execute", r#"{"revision":1}"#)
                .with_response("pooled task graph finished"),
        );
        let mut react_agent = echo_agent::agent::ReactAgentBuilder::new()
            .model("task-pool-path")
            .llm_client(llm.clone())
            .build()
            .map_err(|error| error.to_string())?;
        react_agent.register_subagent_with_definition(
            SubagentDefinition::new("explorer", "Inspect pooled runtime evidence"),
            Box::new(echo_agent::testing::MockAgent::new("explorer").with_response("inspected")),
        );
        let primary_agent = AgentHandle::new(react_agent);
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        crate::tasks::task_runtime::register_task_tools_on_agent(&primary_agent, store.clone())
            .await;
        let pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(primary_agent.clone(), None, None, 3, false)
                .await,
        );
        crate::tasks::task_runtime::bind_task_execute_to_pool(&primary_agent, store.clone(), &pool)
            .await;
        let foreground_execution = pool
            .acquire("pool-conversation")
            .await
            .map_err(|error| error.to_string())?;
        let pooled_agent = foreground_execution.agent();
        let pooled_llm = llm.clone();
        pooled_agent
            .write(move |agent| {
                agent.set_llm_client(pooled_llm);
                agent.register_subagent_with_definition(
                    SubagentDefinition::new("explorer", "Inspect pooled runtime evidence"),
                    Box::new(
                        echo_agent::testing::MockAgent::new("explorer")
                            .with_response("pooled inspection completed"),
                    ),
                );
            })
            .await;
        let pooled_tools = pooled_agent.read(|agent| agent.tool_names()).await;
        for required in ["task_create", "task_execute"] {
            assert!(
                pooled_tools.iter().any(|tool| tool == required),
                "pooled agent is missing required tool {required}"
            );
        }

        let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let sink: Arc<dyn ChatSink> = Arc::new(TaskExecuteReceiptBarrierSink {
            reached: reached_tx,
            release: std::sync::Mutex::new(release_rx),
            parked: std::sync::atomic::AtomicBool::new(false),
        });
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: Some(pool.clone()),
            store: Some(store.clone()),
            sink,
            webhook_emitter: None,
            conv_id: Some("pool-conversation".to_string()),
            root_message_id: "pool-task-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let drive = tokio::spawn(async move {
            drive_chat(
                &pooled_agent,
                &make_turn("run the pooled task graph"),
                resources,
            )
            .await
        });
        let barrier_result = tokio::task::spawn_blocking(move || {
            reached_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| format!("task_execute result barrier was not reached: {error}"))
        })
        .await
        .map_err(|error| error.to_string())?;
        if let Err(error) = barrier_result {
            let early_outcome = tokio::time::timeout(std::time::Duration::from_secs(5), drive)
                .await
                .map_err(|_| {
                    "pooled chat driver did not settle after closing the barrier".to_string()
                })?
                .map_err(|error| error.to_string())?;
            return Err(format!(
                "{error}; outer driver settled as {early_outcome:?} after {} LLM calls",
                llm.call_count()
            ));
        }
        assert_eq!(
            store.active_run_driver_receipt_count()?,
            1,
            "the framework-spawned task_execute pool receipt must belong to the outer driver"
        );
        release_tx
            .send(())
            .map_err(|_| "task_execute result barrier receiver closed".to_string())?;
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), drive)
            .await
            .map_err(|_| "pooled chat driver did not settle".to_string())?
            .map_err(|error| error.to_string())??;
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        drop(foreground_execution);
        tokio::time::timeout(std::time::Duration::from_secs(5), pool.shutdown())
            .await
            .map_err(|_| "pool shutdown timed out after outer settlement".to_string())??;
        Ok(())
    }

    #[tokio::test]
    async fn drive_chat_streams_agent_events_via_sink() -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;
        let mock = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("t")
                .with_response("ok"),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .build()
                .map_err(|error| error.to_string())?,
        );
        let cancel = CancellationToken::new();
        let chat_sink = Arc::new(MockChatSink::default());
        let sink: Arc<dyn ChatSink> = chat_sink.clone();
        let store = Arc::new(
            crate::tasks::task_runtime::store::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let turn_id = "stream-agent-events-contract-turn";
        let res = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(Arc::clone(&store)),
            sink,
            webhook_emitter: None,
            conv_id: Some("c1".to_string()),
            root_message_id: turn_id.to_string(),
            attachments: vec![],
            cancel,
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let outcome = drive_chat(&agent, &make_turn("hi"), res).await?;
        assert_eq!(outcome, TurnOutcome::Completed);
        // The agent's FinalAnswer is streamed through the sink.
        assert!(
            chat_sink.has_final_answer(),
            "drive_chat should stream FinalAnswer to sink; events recorded: {}",
            chat_sink.event_count()
        );
        assert!(
            chat_sink.has_valid_contract("c1", turn_id),
            "drive_chat should preserve version, identity, sequence, and exactly one terminal"
        );
        assert!(
            store
                .get_run(&crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(turn_id),)
                .map_err(|error| error.to_string())?
                .is_none(),
            "ordinary chat must not create a TaskRun"
        );
        assert!(
            !chat_sink.has_run_identity(),
            "ordinary Auto chat events must not claim a nonexistent TaskRun"
        );
        let shutdown_result = store.shutdown_run_drivers().await;
        assert!(
            shutdown_result.is_ok(),
            "optional no-run driver should settle normally: {shutdown_result:?}"
        );
        assert_eq!(store.active_run_driver_count().unwrap_or_default(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn successful_driver_with_failed_input_observer_finishes_once_as_failed()
    -> Result<(), String> {
        let (outcome, sink) = drive_successful_model_with_observer(
            "observer-failed-turn",
            Err("durable input append failed".to_string()),
        )
        .await?;

        assert!(
            sink.has_final_answer(),
            "framework driver did not complete successfully"
        );
        match &outcome.terminal {
            TurnOutcome::Failed(failure) => {
                assert_eq!(failure.code, "input_observer");
                assert!(failure.message.contains("durable input append failed"));
            }
            other => return Err(format!("observer failure finished as {other:?}")),
        }
        assert!(outcome.final_answer.is_some());
        let webhook = WebhookTurnObserver::new(None, "observer-terminal".to_string());
        let mut receipt = test_turn_receipt("observer-failed-turn", TurnOutcome::Completed)?;
        receipt.final_answer = Some("model completed".to_string());
        assert!(!webhook.should_emit_chat_completed(&receipt, &outcome.terminal));
        Ok(())
    }

    #[tokio::test]
    async fn successful_input_observer_preserves_completed_driver_outcome() -> Result<(), String> {
        let (outcome, sink) =
            drive_successful_model_with_observer("observer-success-turn", Ok(())).await?;

        assert!(sink.has_final_answer());
        assert_eq!(outcome.terminal, TurnOutcome::Completed);
        assert_eq!(outcome.final_answer.as_deref(), Some("model completed"));
        let webhook = WebhookTurnObserver::new(None, "observer-terminal".to_string());
        let mut receipt = test_turn_receipt("observer-success-turn", TurnOutcome::Completed)?;
        receipt.final_answer = Some("model completed".to_string());
        assert!(webhook.should_emit_chat_completed(&receipt, &outcome.terminal));
        Ok(())
    }

    #[tokio::test]
    async fn drive_chat_returns_failed_terminal_after_partial_stream_error() -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use echo_agent::llm::types::DeltaMessage;
        use echo_agent::testing::StreamChunk;
        use std::sync::Arc;

        let mock = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("t")
                .with_stream_script(vec![
                    StreamChunk::Delta(DeltaMessage {
                        role: Some("assistant".to_string()),
                        content: Some("partial answer".to_string()),
                        ..DeltaMessage::default()
                    }),
                    StreamChunk::Err(echo_agent::error::ReactError::Other(
                        "provider disconnected".to_string(),
                    )),
                ]),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .build()
                .map_err(|error| error.to_string())?,
        );
        let chat_sink = Arc::new(MockChatSink::default());
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: None,
            sink: chat_sink.clone(),
            webhook_emitter: None,
            conv_id: Some("failure-conversation".to_string()),
            root_message_id: "failure-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });

        let outcome = drive_chat(&agent, &make_turn("start"), resources).await?;
        let TurnOutcome::Failed(failure) = outcome else {
            return Err(format!("expected failed turn, got {outcome:?}"));
        };
        assert!(failure.message.contains("provider disconnected"));

        let events = chat_sink
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let partial_output = events
            .iter()
            .filter_map(|event| match &event.payload {
                AgentEvent::Token(token) => Some(token.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(partial_output, "partial answer");
        assert!(matches!(
            events.last().map(|event| &event.payload),
            Some(AgentEvent::Error { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn drive_chat_projection_survives_snapshot_spawn_and_unregisters() -> Result<(), String> {
        use crate::tasks::task_runtime::compact_context::{
            RUNTIME_RECOVERY_MARKER, task_runtime_projection_registry,
        };
        use crate::tasks::task_runtime::types::{
            AttendedMode, DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, TaskPlan,
        };
        use crate::turn_context::{
            EkoContextProjector, TURN_CONTRACT_MARKER, turn_prompt_context_registry,
        };
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;

        let turn_id = "projection-spawn-boundary";
        let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(turn_id);
        let mock = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("t")
                .with_response("ok"),
        );
        let react_agent = echo_agent::agent::ReactAgentBuilder::new()
            .model("t")
            .llm_client(mock.clone())
            .build()
            .map_err(|error| error.to_string())?;
        react_agent.set_pre_model_context_projector(Some(Arc::new(EkoContextProjector::new(
            task_runtime_projection_registry(),
            turn_prompt_context_registry(),
        ))));
        let agent = AgentHandle::new(react_agent);
        let store = Arc::new(
            crate::tasks::task_runtime::store::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                &run_id,
                "default",
                "c1",
                turn_id,
                DomainProfile::General,
                "boundary goal",
                "complex_runtime",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "boundary-plan".to_string(),
                run_id: run_id.clone(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256("boundary goal"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "boundary-task".to_string(),
                    title: "visible after spawn".to_string(),
                    kind: PlanTaskKind::Investigation,
                    agent_role: "explorer".to_string(),
                    ..PlanTask::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        let chat_sink = Arc::new(MockChatSink::default());
        let sink: Arc<dyn ChatSink> = chat_sink;
        let res = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(Arc::clone(&store)),
            sink,
            webhook_emitter: None,
            conv_id: Some("c1".to_string()),
            root_message_id: turn_id.to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });

        let driver_error = drive_chat(&agent, &make_turn("continue"), res)
            .await
            .err()
            .ok_or_else(|| {
                "projection fixture left a Pending TaskRun but the driver reported success"
                    .to_string()
            })?;
        if !driver_error.contains("non-terminal status pending") {
            return Err(format!(
                "projection fixture returned an unexpected driver error: {driver_error}"
            ));
        }
        let settled_run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "projection fixture run disappeared during settlement".to_string())?;
        if settled_run.status != crate::tasks::task_runtime::TaskRunStatus::Failed {
            return Err(format!(
                "projection fixture did not durably fail its abandoned run: {:?}",
                settled_run.status
            ));
        }

        let messages = mock
            .last_messages()
            .ok_or_else(|| "spawned model call did not receive messages".to_string())?;
        let projected = messages
            .iter()
            .filter_map(|message| message.content.as_text());
        if !projected
            .clone()
            .any(|text| text.contains(RUNTIME_RECOVERY_MARKER))
            || !projected
                .clone()
                .any(|text| text.contains("visible after spawn"))
            || !projected
                .clone()
                .any(|text| text.contains(TURN_CONTRACT_MARKER))
        {
            return Err("EKO projections did not cross snapshot/spawn boundary".to_string());
        }
        if task_runtime_projection_registry().contains(&run_id) {
            return Err("drive_chat did not unregister projection on exit".to_string());
        }
        if turn_prompt_context_registry().contains(turn_id) {
            return Err("drive_chat did not unregister turn contract on exit".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn drive_chat_keeps_user_text_raw_and_projects_execution_contract() -> Result<(), String>
    {
        use echo_agent::agent::CancellationToken;
        use echo_agent::compression::is_context_projection_message;
        use echo_agent::llm::types::Role;
        use std::sync::Arc;

        let mock = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("t")
                .with_response("ok"),
        );
        let react_agent = echo_agent::agent::ReactAgentBuilder::new()
            .model("t")
            .llm_client(mock.clone())
            .build()
            .map_err(|error| error.to_string())?;
        react_agent.set_pre_model_context_projector(Some(Arc::new(
            crate::turn_context::EkoContextProjector::new(
                crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
                crate::turn_context::turn_prompt_context_registry(),
            ),
        )));
        let agent = AgentHandle::new(react_agent);
        let cancel = CancellationToken::new();
        let chat_sink = Arc::new(MockChatSink::default());
        let sink: Arc<dyn ChatSink> = chat_sink.clone();
        let store = Arc::new(
            crate::tasks::task_runtime::store::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let turn_id = "raw-user-text-contract-turn";
        let res = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store),
            sink,
            webhook_emitter: None,
            conv_id: None,
            root_message_id: turn_id.to_string(),
            attachments: vec![],
            cancel,
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let turn = make_turn("hi there");
        drive_chat(&agent, &turn, res).await?;
        let messages = mock
            .last_messages()
            .ok_or_else(|| "LLM was not called".to_string())?;
        let user_messages = messages
            .iter()
            .filter(|message| message.role == Role::User && !is_context_projection_message(message))
            .filter_map(|message| message.content.as_text())
            .collect::<Vec<_>>();
        if user_messages != vec!["hi there"] {
            return Err(format!(
                "non-projection user text was modified: {user_messages:?}"
            ));
        }
        let contract = messages
            .iter()
            .filter(|message| is_context_projection_message(message))
            .filter_map(|message| message.content.as_text())
            .find(|text| text.contains(crate::turn_context::TURN_CONTRACT_MARKER))
            .ok_or_else(|| "turn contract projection missing".to_string())?;
        if !contract.contains("Instruction author: user") {
            return Err(format!("turn contract is incomplete: {contract}"));
        }
        if crate::turn_context::turn_prompt_context_registry().contains(turn_id) {
            return Err("turn contract registration leaked after the turn".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn workspace_execution_scope_reaches_tool_context() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mock = std::sync::Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("scope-test")
                .then_tool_call("scope-call", "web_fetch", "{}")
                .with_response("done"),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("scope-test")
                .llm_client(mock)
                .tool(Box::new(WorkingDirProbeTool {
                    observed: std::sync::Arc::clone(&observed),
                }))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let resources = std::sync::Arc::new(crate::chat_resources::ChatResources {
            execution_scope: crate::workspace::WorkspaceExecutionScope::workspace(
                &crate::workspace::WorkspaceId::from_name("scope-test"),
                root.clone(),
            ),
            workspace_io_receipt: None,
            pool: None,
            store: None,
            sink: std::sync::Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("scope-conversation".to_string()),
            root_message_id: "scope-turn".to_string(),
            attachments: Vec::new(),
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });

        drive_chat(&agent, &make_turn("probe"), resources).await?;
        let actual = observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if actual.as_ref() != Some(&root) {
            return Err(format!(
                "tool observed working directory {actual:?}, expected {root:?}"
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn mismatched_task_runtime_workspace_is_rejected_before_agent_execution()
    -> Result<(), String> {
        let store = std::sync::Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("scope-test")
                .llm_client(std::sync::Arc::new(
                    echo_agent::testing::MockLlmClient::new()
                        .with_model_name("scope-test")
                        .with_response("must not execute"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let resources = std::sync::Arc::new(crate::chat_resources::ChatResources {
            execution_scope: crate::workspace::WorkspaceExecutionScope::global("."),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store),
            sink: std::sync::Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("scope-conversation".to_string()),
            root_message_id: "scope-turn".to_string(),
            attachments: Vec::new(),
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });

        let (observer_tx, observer_rx) = tokio::sync::oneshot::channel();
        let observer_tx = std::sync::Arc::new(tokio::sync::Mutex::new(Some(observer_tx)));
        let input_observer: InputReceiptObserver = std::sync::Arc::new(move |_receipt| {
            let observer_tx = std::sync::Arc::clone(&observer_tx);
            Box::pin(async move {
                if let Some(sender) = observer_tx.lock().await.take() {
                    let _ = sender.send(());
                }
                Ok(())
            })
        });
        let error = drive_chat_turn_with_input_observer(
            &agent,
            &make_turn("probe"),
            resources,
            None,
            Some(input_observer),
        )
        .await
        .err()
        .ok_or_else(|| "mismatched workspace scope was accepted".to_string())?;
        if !error.contains("does not match TaskRuntime workspace") {
            return Err(format!("unexpected scope mismatch error: {error}"));
        }
        let observer = tokio::time::timeout(std::time::Duration::from_secs(1), observer_rx)
            .await
            .map_err(|_| "pre-driver observer sender did not close".to_string())?;
        if observer.is_ok() {
            return Err("pre-driver rejection invoked the input observer".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn channel_chat_sink_forwards_events() -> Result<(), String> {
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::unbounded_channel::<ChatDriverEvent>();
        let sink = ChannelChatSink::new(tx);
        let identity =
            EventIdentity::new("stream-1", "turn-1").map_err(|error| error.to_string())?;

        // on_event forwards each event to the channel and keeps going.
        assert!(
            sink.on_event(ChatDriverEvent::Agent(Box::new(
                EventEnvelope::new(&identity, 1, None, AgentEvent::Token("hel".to_string()),)
                    .map_err(|error| error.to_string())?
            ))),
            "on_event should return true to continue"
        );
        assert!(
            sink.on_event(ChatDriverEvent::Agent(Box::new(
                EventEnvelope::new(&identity, 2, None, AgentEvent::Token("lo".to_string()),)
                    .map_err(|error| error.to_string())?
            ))),
            "second event should also be accepted"
        );

        let first = match rx.recv().await {
            Some(ChatDriverEvent::Agent(event)) => event,
            Some(other) => return Err(format!("first event was not agent event: {other:?}")),
            None => return Err("first event was not forwarded".to_string()),
        };
        let second = match rx.recv().await {
            Some(ChatDriverEvent::Agent(event)) => event,
            Some(other) => return Err(format!("second event was not agent event: {other:?}")),
            None => return Err("second event was not forwarded".to_string()),
        };
        match first.payload {
            AgentEvent::Token(t) => assert_eq!(t, "hel"),
            other => return Err(format!("first should be Token(hel); got {other:?}")),
        }
        match second.payload {
            AgentEvent::Token(t) => assert_eq!(t, "lo"),
            other => return Err(format!("second should be Token(lo); got {other:?}")),
        }
        Ok(())
    }
}
