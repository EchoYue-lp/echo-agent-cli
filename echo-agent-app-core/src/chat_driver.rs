//! Stage 2 — shared chat driver (极简入口).
//!
//! `drive_chat` is the single, thin entry for a chat turn across TUI / CLI
//! channel / GUI: it takes a [`PreparedUserTurn`] (instruction + input
//! resources, with long pastes spilled to a user-input artifact), collapses it
//! into one `Message` via [`PreparedUserTurn::to_message`], and streams the
//! agent's ReAct reply through a surface-specific `ChatSink`. Every turn
//! eagerly drives its own TaskRun (ADR 0037): the default binding carries the
//! turn-derived formal run id, so ordinary chat shares the same admission,
//! projection, and recovery semantics as explicit task submission/resume.
//!
//! Multimodal (images/files) is delivered via the turn's inline resources; the
//! old `(&str, Option<&Message>)` pair has been replaced by the single
//! `PreparedUserTurn`.

pub use crate::tasks::task_runtime::store::RunDriverFailure as ChatTurnError;

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

tokio::task_local! {
    // Only a weak transport of the existing GUI owner. The invocation captures
    // the real lease under the original cell lock, never clones an empty cell
    // as a physical proof or keeps a strong carrier through receipt release.
    static FOREGROUND_POOL_EXECUTION: std::sync::Weak<std::sync::Mutex<Option<crate::agent_pool::AgentPoolExecutionLease>>>;
}

/// Carry an already owned foreground pool receipt through the existing chat
/// entry. This is application lifecycle plumbing, not a durable selection or
/// another admission. Existing callers without a receipt keep their old path.
pub async fn with_foreground_pool_execution<F: std::future::Future>(
    execution: std::sync::Weak<
        std::sync::Mutex<Option<crate::agent_pool::AgentPoolExecutionLease>>,
    >,
    future: F,
) -> F::Output {
    FOREGROUND_POOL_EXECUTION.scope(execution, future).await
}

fn foreground_pool_projection()
-> Result<Option<crate::agent_pool::AgentPoolExecutionProjection>, String> {
    FOREGROUND_POOL_EXECUTION
        .try_with(|execution| {
            let execution = execution
                .upgrade()
                .ok_or("original foreground pool owner is unavailable")?;
            let owner = execution
                .lock()
                .map_err(|_| "original foreground pool receipt is unavailable")?;
            owner
                .as_ref()
                .ok_or("original foreground pool receipt is no longer live")?
                .project_for_invocation()
                .map(Some)
        })
        .unwrap_or(Ok(None))
}

/// Complete product event stream consumed by every interactive surface.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "ChatAttachmentDescriptor")]
pub struct ChatAttachmentDescriptor {
    pub name: String,
    pub mime_type: String,
    pub source: crate::types::AttachmentSource,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "source", content = "event", rename_all = "snake_case")]
pub enum ChatDriverEvent {
    Agent(Box<EventEnvelope>),
    Execution(Box<ExecEvent>),
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
    /// Backend-only control lifecycle; payload is a digest, never the command or HITL answer.
    WeixinControlDelivery(Box<echo_agent::state::delivery::DeliveryEvent<String, String>>),
    ApprovalRequest {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<crate::hitl::HitlRequestIdentity>,
        tool_name: String,
        args: serde_json::Value,
        prompt: String,
    },
    InputRequest {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<crate::hitl::HitlRequestIdentity>,
        prompt: String,
    },
    SelectionRequest {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<crate::hitl::HitlRequestIdentity>,
        prompt: String,
        options: Vec<String>,
        task_id: Option<String>,
        context: Option<serde_json::Value>,
        phase: Option<String>,
    },
    /// Projection removal derived from the original provider reservation Drop.
    /// This is neither an approval decision nor another terminal authority.
    HitlRequestReleased {
        identity: crate::hitl::HitlRequestIdentity,
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
    CommandCellWatchReady {
        result: Box<crate::tasks::task_runtime::command_cells::CommandCellWatchResult>,
    },
    CommandCellWatchDeliveryStarted {
        acknowledgement: crate::tasks::task_runtime::command_cells::CommandCellWatchAcknowledgement,
    },
    CommandCellWatchAcknowledged {
        acknowledgement: crate::tasks::task_runtime::command_cells::CommandCellWatchAcknowledgement,
    },
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
        let _ = sink.on_event(ChatDriverEvent::Execution(Box::new(event)));
    })
}

/// Bridge the framework's JSON trace transport into the same product stream.
pub fn framework_trace_sink_for(sink: &std::sync::Arc<dyn ChatSink>) -> TraceSinkFn {
    let sink = std::sync::Arc::clone(sink);
    std::sync::Arc::new(
        move |value| match serde_json::from_value::<ExecEvent>(value) {
            Ok(event) => {
                let _ = sink.on_event(ChatDriverEvent::Execution(Box::new(event)));
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

    fn finish(self, mut receipt: TurnReceipt, terminal: TurnOutcome) -> TurnReceipt {
        if self.should_emit_chat_completed(&receipt, &terminal)
            && let Some(emitter) = self.emitter
        {
            emitter.emit(crate::webhook::WebhookEvent::ChatCompleted {
                model: self.model,
                input_tokens: usize::try_from(receipt.prompt_tokens).unwrap_or(usize::MAX),
                output_tokens: usize::try_from(receipt.completion_tokens).unwrap_or(usize::MAX),
                elapsed_ms: duration_millis(receipt.elapsed),
            });
        }
        receipt.outcome = terminal;
        receipt
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

#[cfg(test)]
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
/// Every turn eagerly drives its own `taskrun:<turn_id>` TaskRun (ADR 0037);
/// explicit bindings from task submission/resume paths pin an existing run.
///
/// ## turn/run identity
///
/// 普通 chat 轮次使用 `res.root_message_id` 作 turn_id,并从该 turn_id
/// 派生 `taskrun:<turn_id>`。Task tools 在同一 run 内提交正式 plan。这样
/// 主 agent 在 ReAct 循环里调
/// `task_create` / `task_execute` / `create_complex_task` 等依赖
/// `require_run_id()` 的工具时,能从 task_local 读到 run_id,不再被
/// `"no active run — run_id not set in context"` 提前拒绝(对齐 Claude Code
/// 的无门槛只读 dispatch)。
///
/// turn_id 与 exact run_id 一起进入 Agent ExternalRunContext。独立的
/// `create_complex_task` 仍拥有自己的 orchestrated run_id。
pub async fn drive_chat(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
) -> Result<TurnOutcome, ChatTurnError> {
    drive_chat_turn(agent, turn, res, None)
        .await
        .map(|receipt| receipt.outcome)
}

/// Drive one finite invocation with an optional existing TaskRun binding.
/// This is the only detailed driver; [`drive_chat`] is the surface-compatible
/// terminal projection of the same path. The detailed path returns the
/// framework-owned [`TurnReceipt`] directly so callers retain exact identity,
/// usage, sequence, and elapsed-time facts.
pub async fn drive_chat_turn(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
) -> Result<TurnReceipt, ChatTurnError> {
    drive_chat_turn_with_input_observer(agent, turn, res, binding, None).await
}

pub async fn drive_chat_turn_with_input_observer(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
    input_observer: Option<InputReceiptObserver>,
) -> Result<TurnReceipt, ChatTurnError> {
    drive_chat_turn_with_input_source(agent, turn, res, binding, input_observer, None).await
}

/// 同一应用driver的不可变输入载体；不修改framework TurnRequest或另建输入owner。
pub async fn drive_chat_turn_with_input_source(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
    input_observer: Option<InputReceiptObserver>,
    input_source: Option<crate::conversation_input::BoundRunInput>,
) -> Result<TurnReceipt, ChatTurnError> {
    match drive_chat_turn_dispatch_with_source(
        agent,
        turn,
        res,
        binding,
        input_observer,
        input_source,
    )
    .await?
    {
        RunTurnDriveOutcome::Driven(receipt) => Ok(*receipt),
        RunTurnDriveOutcome::Deferred => {
            Err("RunTurn continuation deferred before model execution".into())
        }
    }
}

pub(crate) async fn drive_chat_continuation_turn(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: RunTurnBinding,
) -> Result<RunTurnDriveOutcome, ChatTurnError> {
    drive_chat_turn_dispatch(agent, turn, res, Some(binding), None).await
}

async fn drive_chat_turn_dispatch(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
    input_observer: Option<InputReceiptObserver>,
) -> Result<RunTurnDriveOutcome, ChatTurnError> {
    drive_chat_turn_dispatch_with_source(agent, turn, res, binding, input_observer, None).await
}

async fn drive_chat_turn_dispatch_with_source(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
    input_observer: Option<InputReceiptObserver>,
    input_source: Option<crate::conversation_input::BoundRunInput>,
) -> Result<RunTurnDriveOutcome, ChatTurnError> {
    wait_for_previous_continuation_driver(&res, binding.as_ref()).await?;
    match prepare_chat_execution_with_source(turn, res, binding, input_source).await? {
        ChatExecutionPreparation::Ready(prepared) => {
            drive_prepared_chat(agent.clone(), turn, *prepared, None, input_observer)
                .await
                .map(|receipt| RunTurnDriveOutcome::Driven(Box::new(receipt)))
        }
        ChatExecutionPreparation::Settled(outcome) => Ok(RunTurnDriveOutcome::Driven(outcome)),
        ChatExecutionPreparation::Deferred => Ok(RunTurnDriveOutcome::Deferred),
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
) -> Result<TurnOutcome, ChatTurnError>
where
    Configure: FnOnce(AgentHandle) -> ConfigureFuture,
    ConfigureFuture: std::future::Future<Output = Result<(), String>>,
{
    drive_pooled_chat_turn(pool, pool_key, configure, turn, res, None)
        .await
        .map(|receipt| receipt.outcome)
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
) -> Result<TurnReceipt, ChatTurnError>
where
    Configure: FnOnce(AgentHandle) -> ConfigureFuture,
    ConfigureFuture: std::future::Future<Output = Result<(), String>>,
{
    match drive_pooled_chat_turn_dispatch(pool, pool_key, configure, turn, res, binding.into())
        .await?
    {
        RunTurnDriveOutcome::Driven(receipt) => Ok(*receipt),
        RunTurnDriveOutcome::Deferred => {
            Err("RunTurn continuation deferred before model execution".into())
        }
    }
}

pub(crate) async fn drive_pooled_chat_continuation_turn<Configure, ConfigureFuture>(
    pool: std::sync::Arc<crate::agent_pool::AgentPool>,
    pool_key: &str,
    configure: Configure,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: RunTurnBinding,
) -> Result<RunTurnDriveOutcome, ChatTurnError>
where
    Configure: FnOnce(AgentHandle) -> ConfigureFuture,
    ConfigureFuture: std::future::Future<Output = Result<(), String>>,
{
    drive_pooled_chat_turn_dispatch(pool, pool_key, configure, turn, res, Some(binding)).await
}

async fn drive_pooled_chat_turn_dispatch<Configure, ConfigureFuture>(
    pool: std::sync::Arc<crate::agent_pool::AgentPool>,
    pool_key: &str,
    configure: Configure,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
) -> Result<RunTurnDriveOutcome, ChatTurnError>
where
    Configure: FnOnce(AgentHandle) -> ConfigureFuture,
    ConfigureFuture: std::future::Future<Output = Result<(), String>>,
{
    wait_for_previous_continuation_driver(&res, binding.as_ref()).await?;
    let mut prepared = match prepare_chat_execution(turn, res, binding).await? {
        ChatExecutionPreparation::Ready(prepared) => prepared,
        ChatExecutionPreparation::Settled(outcome) => {
            return Ok(RunTurnDriveOutcome::Driven(outcome));
        }
        ChatExecutionPreparation::Deferred => return Ok(RunTurnDriveOutcome::Deferred),
    };
    let acquire_cancel = prepared.cancel.clone();
    let execution = if let Some(execution) = prepared.domain_admission.take() {
        execution
    } else {
        match tokio::select! {
            biased;
            _ = acquire_cancel.cancelled() => {
                prepared
                    .reject_before_driver_start("continuation cancelled during pool admission")
                    .await?;
                return Err("continuation cancelled during pool admission".into());
            }
            result = pool.acquire_for_turn(pool_key) => result,
        } {
            Ok(execution) => execution,
            Err(error) => {
                let message = format!("AgentPool admission failed: {error}");
                prepared.reject_before_driver_start(&message).await?;
                return Err(message.into());
            }
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
            return Err("continuation cancelled during agent configuration".into());
        }
        result = configure(agent.clone()) => result,
    };
    if let Err(error) = configuration {
        prepared.reject_before_driver_start(&error).await?;
        return Err(error.into());
    }
    drive_prepared_chat(agent, turn, *prepared, Some(execution), None)
        .await
        .map(|receipt| RunTurnDriveOutcome::Driven(Box::new(receipt)))
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
    Settled(Box<TurnReceipt>),
    Deferred,
}

#[derive(Debug)]
pub(crate) enum RunTurnDriveOutcome {
    Driven(Box<TurnReceipt>),
    Deferred,
}

struct PreparedChatExecution {
    turn_id: String,
    formal_run_id: String,
    binding: RunTurnBinding,
    task_driver_registration:
        Option<crate::tasks::task_runtime::store::RegisteredRunDriver<TurnReceipt, ChatTurnError>>,
    resources: std::sync::Arc<crate::chat_resources::ChatResources>,
    cancel: echo_agent::agent::CancellationToken,
    sink: std::sync::Arc<dyn ChatSink>,
    trace_sink: crate::tasks::task_runtime::task_tools::TraceSink,
    drives_task_run: bool,
    store: Option<std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    foreground_progress: Option<crate::foreground_turn::ForegroundTurnProgress>,
    domain_admission: Option<crate::agent_pool::AgentPoolExecutionLease>,
    domain_projection: Option<crate::agent_pool::AgentPoolExecutionProjection>,
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
        let blocking = crate::tasks::task_runtime::TaskRuntimeOperation::new(store.clone());
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

fn check_resume_domain_selection(
    requested: Option<&str>,
    prepared: Option<&crate::domain_plugin::DomainBinding>,
    original: Option<&crate::domain_plugin::DomainBinding>,
) -> Result<(), String> {
    let Some(requested) = requested else {
        return Ok(());
    };
    let Some(original) = original else {
        return Err("an existing General TaskRun cannot acquire a domain through resume; create a new domain-bound task".to_string());
    };
    if original.snapshot.plugin.plugin_id != requested
        || prepared.is_some_and(|prepared| prepared != original)
    {
        return Err(
            "explicit resume domain selection conflicts with the original TaskRun binding"
                .to_string(),
        );
    }
    Ok(())
}

async fn prepare_chat_execution(
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
) -> Result<ChatExecutionPreparation, ChatTurnError> {
    prepare_chat_execution_with_source(turn, res, binding, None).await
}

async fn prepare_chat_execution_with_source(
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
    input_source: Option<crate::conversation_input::BoundRunInput>,
) -> Result<ChatExecutionPreparation, ChatTurnError> {
    let foreground_progress = crate::foreground_turn::current_foreground_progress();
    if let Some(store) = res.store.as_ref() {
        let active_workspace_id = store.active_workspace_id();
        if active_workspace_id != res.execution_scope.workspace_id() {
            return Err(format!(
                "Chat execution scope {} does not match TaskRuntime workspace {}",
                res.execution_scope.workspace_id(),
                active_workspace_id
            )
            .into());
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
    // ADR 0037: every turn eagerly drives its own TaskRun. The default
    // binding carries the turn-derived formal run id so ordinary chat goes
    // through the same admission, projection, and recovery semantics as
    // explicit task/resume turns; a run that never commits a plan revision
    // settles as Completed at turn end instead of entering continuation.
    // A store-less degraded execution (task runtime not yet bound) physically
    // cannot own a TaskRun and keeps the legacy unbound path.
    let mut binding = binding.unwrap_or_else(|| RunTurnBinding {
        domain_plugin: None,
        run_id: res.store.is_some().then(|| {
            crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&default_turn_id)
        }),
        turn_id: default_turn_id.clone(),
        root_message_id: default_turn_id,
        origin: RunTurnOrigin::User,
        transcript_visibility: TurnVisibility::Visible,
        expected_resume: None,
    });
    // Normalize any caller-supplied run-less binding the same way so no
    // store-backed turn can execute outside the unified runtime (ADR 0037).
    if binding.run_id.is_none() && res.store.is_some() {
        binding.run_id =
            Some(crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&binding.turn_id));
    }
    if binding.turn_id.trim().is_empty() || binding.root_message_id.trim().is_empty() {
        return Err("RunTurn binding requires non-empty turn and root message ids".into());
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
            )
            .into());
        }
    }
    let turn_id = binding.turn_id.clone();
    let drives_task_run = binding.run_id.is_some();
    let formal_run_id = binding.run_id.clone().unwrap_or_else(|| {
        crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&turn_id)
    });
    if let Some(input) = input_source.as_ref() {
        input.snapshot.validate()?;
        if !drives_task_run
            || input.target.run_id != formal_run_id
            || input.target.turn_id != turn_id
            || input.target.identity != input.snapshot.identity
            || input.snapshot.identity.address.workspace_id != res.execution_scope.workspace_id()
            || res.conv_id.as_deref()
                != Some(input.snapshot.identity.address.conversation_id.as_str())
        {
            return Err("原输入来源与准确TaskRun执行作用域不匹配，未执行".into());
        }
    }
    // Reject a conflicting explicit selection alongside the original binding
    // checks, before registering an execution owner for an invalid request.
    let resume_domain = if binding.expected_resume.is_some() && binding.domain_plugin.is_some() {
        let store = res
            .store
            .as_ref()
            .ok_or_else(|| "domain resume requires its original TaskRun owner".to_string())?;
        let snapshot = store
            .get_run_state(&formal_run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "domain resume original TaskRun is missing".to_string())?;
        check_resume_domain_selection(
            binding.domain_plugin.as_deref(),
            None,
            snapshot.domain_binding.as_ref(),
        )?;
        snapshot.domain_binding
    } else {
        None
    };
    // Every turn registers its run driver before memory admission; with the
    // eager default binding (ADR 0037) every ordinary turn owns a formal
    // TaskRun from the first model boundary.
    // Store admission can observe an original unresolved physical owner.
    // Preserve that existing typed cause; context belongs only to ordinary
    // errors, which retain their original presentation and settlement.
    let admission_error = |context: &str, error| match ChatTurnError::from(error) {
        ChatTurnError::Error(message) => ChatTurnError::Error(format!("{context}: {message}")),
        observation => observation,
    };
    let mut task_driver_registration = match res.store.as_ref() {
        Some(store) => {
            let admission = store
                .reserve_run_driver_admission(formal_run_id.clone(), res.cancel.clone())
                .map_err(|error| admission_error("chat driver admission failed", error))?;
            let generation = store.lease_active_workspace_generation().map_err(|error| {
                admission_error("task runtime generation admission failed", error)
            })?;
            let registration = store
                .register_observed_run_driver::<TurnReceipt>(admission, generation)
                .map_err(|error| admission_error("chat driver registration failed", error))?;
            Some(registration)
        }
        None if drives_task_run => {
            return Err("TaskRun-bound turn requires TaskRuntimeStore".into());
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
            return Ok(ChatExecutionPreparation::Settled(Box::new(
                TurnReceipt::failed(turn_id, failure).map_err(|error| error.to_string())?,
            )));
        }
    };
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
        memory_generation,
        human_loop_provider: res.human_loop_provider.clone(),
    });
    let cancel = res.cancel.clone();
    let sink = res.sink.clone();
    let trace_sink = subagent_trace_sink_for(&sink);
    let store = res.store.clone();
    let mut domain_projection = None;
    let (domain_admission, selected_domain) =
        if let Some(plugin_id) = binding.domain_plugin.as_deref() {
            if plugin_id.trim().is_empty() || !drives_task_run {
                return Err("domain selection requires a nonempty plugin id and TaskRun".into());
            }
            let pool = res
                .pool
                .as_ref()
                .ok_or_else(|| "domain selection requires the original AgentPool".to_string())?;
            let key = res
                .conv_id
                .as_deref()
                .ok_or_else(|| "domain selection requires a conversation identity".to_string())?;
            if let Some(projection) = foreground_pool_projection()? {
                let selected = pool.bind_domain(projection.execution(), plugin_id).await?;
                domain_projection = Some(projection);
                (None, Some(selected))
            } else {
                let lease = match pool
                    .lease_existing(key)
                    .await
                    .map_err(|error| error.to_string())?
                {
                    Some(lease) => lease,
                    None => pool
                        .acquire_for_turn(key)
                        .await
                        .map_err(|error| error.to_string())?,
                };
                let selected = pool.bind_domain(&lease, plugin_id).await?;
                (Some(lease), Some(selected))
            }
        } else {
            (None, None)
        };
    if binding.expected_resume.is_some() {
        check_resume_domain_selection(
            binding.domain_plugin.as_deref(),
            selected_domain.as_ref(),
            resume_domain.as_ref(),
        )?;
    }
    if drives_task_run {
        let registration = task_driver_registration.take().ok_or_else(|| {
            "TaskRun-bound turn lost its driver registration during preparation".to_string()
        })?;
        // 原输入载体保存完整Goal；PreparedUserTurn继续只负责模型使用的引用和资源。
        let task_store = store.as_ref().ok_or_else(|| {
            "TaskRun-bound turn lost its TaskRuntimeStore during preparation".to_string()
        })?;
        let owned_run_id = formal_run_id.clone();
        let owned_conversation_id = res.conv_id.clone();
        let owned_root_message_id = binding.root_message_id.clone();
        let owned_instruction = input_source
            .as_ref()
            .map(|input| input.snapshot.payload.text.clone())
            .unwrap_or_else(|| turn.instruction.clone());
        let owned_input_source = input_source.as_ref().map(|input| input.snapshot.clone());
        let owned_attachments = res.attachments.clone();
        let expected_resume = binding.expected_resume.clone();
        let origin = binding.origin;
        let visibility = binding.transcript_visibility;
        let owned_turn_id = turn_id.clone();
        let owned_trace_sink = trace_sink.clone();
        let owned_store = task_store.clone();
        let selected_domain = selected_domain.clone();
        let claim = crate::tasks::task_runtime::TaskRuntimeOperation::new(task_store.clone())
            .run_owned("prepare and claim chat TaskRun", move || {
                let mut registration = registration;
                registration.mark_preparation_started();
                if expected_resume.is_none() {
                    if let Err(error) = ensure_bound_task_run_with_domain_source(
                        &owned_store,
                        &owned_run_id,
                        owned_conversation_id.as_deref(),
                        &owned_root_message_id,
                        &owned_instruction,
                        &owned_attachments,
                        Some(&owned_trace_sink),
                        selected_domain.as_ref(),
                        owned_input_source.as_ref(),
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
                    match owned_input_source.as_ref() {
                        Some(input) => owned_store.resume_and_claim_run_turn_expected_with_input(
                            expected,
                            &owned_turn_id,
                            origin,
                            visibility,
                            input,
                        ),
                        None => owned_store.resume_and_claim_run_turn_expected(
                            expected,
                            &owned_turn_id,
                            origin,
                            visibility,
                        ),
                    }
                } else if origin == RunTurnOrigin::Resume {
                    Err(crate::tasks::task_runtime::StoreError::InvalidPlan(
                        "resume RunTurn requires an exact queued TaskRun identity".to_string(),
                    ))
                } else {
                    owned_store.claim_run_turn(&owned_run_id, &owned_turn_id, origin, visibility)
                };
                Ok((registration, claim))
            })
            .await;
        let claim = match claim {
            Ok((registration, Ok(claim))) => {
                task_driver_registration = Some(registration);
                claim
            }
            Ok((registration, Err(error))) => {
                if matches!(
                    &error,
                    crate::tasks::task_runtime::StoreError::ResumeOutcomeUnknown { .. }
                ) {
                    registration.fail_preparation(error.to_string());
                } else {
                    registration.reject(error.to_string());
                }
                return Err(error.into());
            }
            Err(error) => {
                return Err(error.into());
            }
        };
        match claim {
            crate::tasks::task_runtime::store::RunTurnClaimOutcome::Started(_) => {}
            crate::tasks::task_runtime::store::RunTurnClaimOutcome::NotSubmitted(reason) => {
                let deferred = reason
                    == crate::tasks::task_runtime::store::ContinuationNotSubmittedReason::Deferred
                    && matches!(
                        origin,
                        RunTurnOrigin::Continuation | RunTurnOrigin::Recovery
                    );
                if let Some(registration) = task_driver_registration.take() {
                    registration.reject(format!("RunTurn was not submitted: {reason:?}"));
                }
                if deferred {
                    return Ok(ChatExecutionPreparation::Deferred);
                }
                return Err(format!("RunTurn was not submitted: {reason:?}").into());
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
            domain_admission,
            domain_projection,
        },
    )))
}

async fn drive_prepared_chat(
    agent: AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    mut prepared: PreparedChatExecution,
    pool_execution: Option<crate::agent_pool::AgentPoolExecutionLease>,
    input_observer: Option<InputReceiptObserver>,
) -> Result<TurnReceipt, ChatTurnError> {
    let mut pool_execution = pool_execution.or_else(|| prepared.domain_admission.take());
    let mut domain_binding = None;
    if let Some(store) = prepared.store.as_ref() {
        let snapshot = store
            .get_run_state(&prepared.formal_run_id)
            .map_err(|error| error.to_string())?;
        if let Some(snapshot) = snapshot {
            if let Some(binding) = snapshot.domain_binding.as_ref() {
                domain_binding = Some(binding.clone());
                let check = async {
                    let pool = prepared.resources.pool.as_ref().ok_or_else(|| {
                        "domain execution requires its original AgentPool".to_string()
                    })?;
                    if pool_execution.is_none() && prepared.domain_projection.is_none() {
                        prepared.domain_projection = foreground_pool_projection()?;
                    }
                    if pool_execution.is_none() && prepared.domain_projection.is_none() {
                        let key = prepared.resources.conv_id.as_deref().ok_or_else(|| {
                            "domain execution lost its conversation identity".to_string()
                        })?;
                        pool_execution = pool
                            .lease_existing(key)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    let lease = pool_execution
                        .as_ref()
                        .or_else(|| {
                            prepared
                                .domain_projection
                                .as_ref()
                                .map(|projection| projection.execution())
                        })
                        .ok_or_else(|| {
                            "domain execution has no original live pool lease".to_string()
                        })?;
                    if !std::sync::Arc::ptr_eq(agent.inner(), lease.agent().inner()) {
                        return Err(
                            "domain invocation Agent does not match its original execution lease"
                                .to_string(),
                        );
                    }
                    pool.verify_domain_binding(lease, binding).await
                }
                .await;
                if let Err(error) = check {
                    prepared.reject_before_driver_start(&error).await?;
                    return Err(error.into());
                }
            } else if snapshot.run.domain_profile
                != crate::tasks::task_runtime::DomainProfile::General
            {
                let error = "historical professional TaskRun has no trusted plugin binding; create a new domain-bound task";
                prepared.reject_before_driver_start(error).await?;
                return Err(error.into());
            }
        }
    }
    let run_projection = match (domain_binding.as_ref(), prepared.store.as_ref()) {
        (Some(_), Some(store)) => Some(std::sync::Arc::new(
            crate::tasks::task_runtime::compact_context::task_runtime_projection_registry()
                .retain_for_invocation(&prepared.formal_run_id, store.clone())?,
        )),
        _ => None,
    };
    let original_execution = pool_execution.as_ref().or_else(|| {
        prepared
            .domain_projection
            .as_ref()
            .map(|projection| projection.execution())
    });
    let context_initialization = match original_execution {
        Some(execution) => execution.context_initialization_for(&agent),
        None => foreground_pool_projection()?
            .and_then(|projection| projection.execution().context_initialization_for(&agent)),
    };
    let invocation_resources = match (domain_binding.as_ref(), original_execution) {
        (Some(binding), Some(execution)) => vec![execution.domain_resource_guard(
            &prepared.formal_run_id,
            binding,
            run_projection.clone(),
        )?],
        (Some(_), None) => return Err("domain invocation lost its physical pool receipt".into()),
        _ => Vec::new(),
    };
    // Preparation may observe the already owned GUI lease, but it must not
    // retain a control-stack proof while the supervisor awaits physical release.
    // The actual invocation guard now holds the required original accounting.
    drop(prepared.domain_projection.take());
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
    let launcher_cut = if prepared.drives_task_run
        && !continuation_dispatch_owned
        && let Some(store) = prepared.store.as_ref()
    {
        Some(crate::tasks::task_runtime::continuation::register_launcher(
            store,
            &prepared.formal_run_id,
            agent.clone(),
            prepared.resources.clone(),
            prepared.binding.root_message_id.clone(),
            prepared.foreground_progress.clone(),
        ))
    } else {
        None
    };
    let result = if let Some(registration) = prepared.task_driver_registration.take() {
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
                receipt_owner.retain(generation.clone())?;
            }
            if let Some(execution) = pool_execution {
                receipt_owner.retain(execution)?;
            }
            let _projection_registration = run_projection.unwrap_or_else(|| {
                std::sync::Arc::new(
                    crate::tasks::task_runtime::compact_context::task_runtime_projection_registry()
                        .register(owned_run_id.clone(), store.clone()),
                )
            });
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
                invocation_resources,
                context_initialization,
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
                    invocation_resources,
                    context_initialization,
                },
                input_observer,
            ),
        )
        .await
    };
    if result
        .as_ref()
        .err()
        .is_some_and(ChatTurnError::is_observation)
    {
        return result;
    }
    if prepared.drives_task_run
        && !continuation_dispatch_owned
        && let Some(store) = prepared.store.as_ref()
    {
        let status = store
            .get_run(&prepared.formal_run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!(
                    "TaskRun {} disappeared after its turn",
                    prepared.formal_run_id
                )
            })?
            .status;
        if status == crate::tasks::task_runtime::TaskRunStatus::Running {
            let outcome = crate::tasks::task_runtime::continuation::request_continue(
                store,
                &prepared.formal_run_id,
                RunTurnOrigin::Continuation,
            );
            if let crate::tasks::task_runtime::continuation::ContinueRequestOutcome::Running(
                request,
            ) = outcome
            {
                tracing::debug!(
                    run_id = %prepared.formal_run_id,
                    disposition = ?request.disposition,
                    "finite RunTurn requested continuation"
                );
                if let Some(progress) = prepared.foreground_progress.as_ref() {
                    let completion = request.completion.wait().await?;
                    // completion已等待原driver物理idle。成功还须读同generation的
                    // 原Run；Completed的Deferred/Paused回执不能冒成Goal完成。
                    let terminal = if completion.terminal != TurnOutcome::Completed {
                        Some(completion.terminal.clone())
                    } else if completion.reason == crate::tasks::task_runtime::continuation::ContinuationCompletionReason::Stopped {
                        let run_id = prepared.formal_run_id.clone();
                        let state = crate::tasks::task_runtime::TaskRuntimeOperation::new(store.clone())
                            .run_store("read original foreground continuation completion", move |store| store.get_run_state(&run_id))
                            .await.map_err(|error| error.to_string())?
                            .ok_or_else(|| "foreground continuation original run disappeared".to_string())?;
                        foreground_continuation_projection(
                            &completion, &state.run, &prepared.formal_run_id,
                            &prepared.binding, &prepared.resources,
                        )?
                    } else {
                        None
                    };
                    if result.is_ok()
                        && let Some(terminal) = terminal
                    {
                        // 只回传原前台surface投影，首finite receipt与已存journal保持原值。
                        progress.record_continuation_outcome(&prepared.formal_run_id, terminal)?;
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
                    "running RunTurn has no continuation launcher"
                );
                if prepared.foreground_progress.is_some() {
                    return Err(format!(
                        "foreground TaskRun {} lost its continuation launcher",
                        prepared.formal_run_id
                    )
                    .into());
                }
            }
        } else if matches!(
            status,
            crate::tasks::task_runtime::TaskRunStatus::Completed
                | crate::tasks::task_runtime::TaskRunStatus::Cancelled
                | crate::tasks::task_runtime::TaskRunStatus::Failed
        ) && let Some(cut) = launcher_cut
        {
            // 首回合直接结束时不会进入 continuation dispatch；仍须退休本回合
            // 注册的被动 launcher，释放 renderer。原执行结果已返回，Observation
            // 已在上方保留；原 generation 回执保护后继注册及其物理所有者。
            crate::tasks::task_runtime::continuation::clear_launcher_at_cut(
                store,
                &prepared.formal_run_id,
                cut,
            );
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

/// 从原completion与Run事实派生前台呈现；不重新判定执行/验收成功，不写原事实。
fn foreground_continuation_projection(
    completion: &crate::tasks::task_runtime::continuation::ContinuationCompletion,
    run: &crate::tasks::task_runtime::types::TaskRun,
    run_id: &str,
    binding: &RunTurnBinding,
    resources: &crate::chat_resources::ChatResources,
) -> Result<Option<TurnOutcome>, String> {
    use crate::tasks::task_runtime::{TaskRunStatus, continuation::ContinuationCompletionReason};
    if completion.reason != ContinuationCompletionReason::Stopped
        || completion.terminal != TurnOutcome::Completed
    {
        return Ok(None);
    }
    if run.run_id != run_id
        || run.workspace_id != resources.execution_scope.workspace_id()
        || resources.conv_id.as_deref() != Some(run.conversation_id.as_str())
        || run.root_message_id != binding.root_message_id
    {
        return Err(
            "foreground continuation original run identity changed; success not published"
                .to_string(),
        );
    }
    Ok((run.status == TaskRunStatus::Completed).then_some(TurnOutcome::Completed))
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
    invocation_resources: Vec<echo_agent::tools::InvocationResourceGuard>,
    context_initialization: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

async fn drive_registered_turn(
    mut driver: RegisteredTurnDriver,
) -> Result<TurnReceipt, ChatTurnError> {
    if let Some(progress) = driver.foreground_progress.as_ref() {
        progress.bind_run_turn(&driver.run_id, &driver.binding.turn_id);
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
                invocation_resources: std::mem::take(&mut driver.invocation_resources),
                context_initialization: driver.context_initialization.take(),
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
            if error.is_observation() {
                return result;
            }
            fallback = TurnReceipt::failed(
                driver.binding.turn_id,
                echo_agent::error::AgentFailure::message("chat_driver", error.to_string()),
            )
            .map_err(|failure| failure.to_string())?;
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

#[cfg(test)]
fn ensure_bound_task_run(
    store: &crate::tasks::task_runtime::TaskRuntimeStore,
    run_id: &str,
    conversation_id: Option<&str>,
    turn_id: &str,
    goal: &str,
    attachments: &[crate::attachments::AttachmentRef],
    trace_sink: Option<&crate::tasks::task_runtime::task_tools::TraceSink>,
) -> Result<(), crate::tasks::task_runtime::StoreError> {
    ensure_bound_task_run_with_domain(
        store,
        run_id,
        conversation_id,
        turn_id,
        goal,
        attachments,
        trace_sink,
        None,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn ensure_bound_task_run_with_domain(
    store: &crate::tasks::task_runtime::TaskRuntimeStore,
    run_id: &str,
    conversation_id: Option<&str>,
    turn_id: &str,
    goal: &str,
    attachments: &[crate::attachments::AttachmentRef],
    trace_sink: Option<&crate::tasks::task_runtime::task_tools::TraceSink>,
    selected_domain: Option<&crate::domain_plugin::DomainBinding>,
) -> Result<(), crate::tasks::task_runtime::StoreError> {
    ensure_bound_task_run_with_domain_source(
        store,
        run_id,
        conversation_id,
        turn_id,
        goal,
        attachments,
        trace_sink,
        selected_domain,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn ensure_bound_task_run_with_domain_source(
    store: &crate::tasks::task_runtime::TaskRuntimeStore,
    run_id: &str,
    conversation_id: Option<&str>,
    turn_id: &str,
    goal: &str,
    attachments: &[crate::attachments::AttachmentRef],
    trace_sink: Option<&crate::tasks::task_runtime::task_tools::TraceSink>,
    selected_domain: Option<&crate::domain_plugin::DomainBinding>,
    input_source: Option<&crate::tasks::task_runtime::RunInputSnapshot>,
) -> Result<(), crate::tasks::task_runtime::StoreError> {
    use crate::tasks::task_runtime::{AttendedMode, DomainProfile, TaskRunStatus};

    let existing = store.get_run_state(run_id)?;
    // A continuation consumes the original run identity. Its internal prompt
    // is not a new attended request and must not rewrite route or domain.
    // The existing store validator still checks workspace/conversation/root.
    let (domain_profile, route, attended_mode, run_goal, execution_profile) =
        match existing.as_ref() {
            Some(snapshot) => (
                snapshot.run.domain_profile,
                snapshot.run.route.as_str(),
                snapshot.run.attended_mode,
                snapshot.run.goal.as_str(),
                snapshot.execution_profile,
            ),
            None => (
                DomainProfile::General,
                "agent_task_plan",
                AttendedMode::Attended,
                goal,
                selected_domain.map_or_else(
                    crate::tasks::task_runtime::TaskRunExecutionProfile::conversation_turn,
                    |binding| {
                        crate::tasks::task_runtime::TaskRunExecutionProfile::orchestrated(
                            if binding.requires_plan() {
                                crate::tasks::task_runtime::RunPlanPolicy::RequirePlan
                            } else {
                                crate::tasks::task_runtime::RunPlanPolicy::AllowDirect
                            },
                        )
                    },
                ),
            ),
        };
    let persisted_domain = existing
        .as_ref()
        .and_then(|state| state.domain_binding.as_ref());
    let effective_domain = selected_domain.or(persisted_domain);
    let run = if let Some(input) = input_source {
        store.create_run_for_active_workspace_with_source(
            run_id,
            conversation_id.unwrap_or("message:task"),
            turn_id,
            domain_profile,
            run_goal,
            route,
            attended_mode,
            execution_profile,
            effective_domain,
            input,
        )?
    } else {
        store.create_run_for_active_workspace_with_domain(
            run_id,
            conversation_id.unwrap_or("message:task"),
            turn_id,
            domain_profile,
            run_goal,
            route,
            attended_mode,
            execution_profile,
            effective_domain,
        )?
    };
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
                    "goal": &run.goal,
                    "route": &run.route,
                }),
            ));
        }
    }
    Ok(())
}

async fn finalize_run_turn(
    store: &std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
    run_id: &str,
    outcome: &TurnReceipt,
    trace_sink: Option<&crate::tasks::task_runtime::task_tools::TraceSink>,
) -> Result<crate::tasks::task_runtime::turn_lifecycle::RunTurnDecision, String> {
    let blocking = crate::tasks::task_runtime::TaskRuntimeOperation::new(store.clone());
    let policy_run_id = run_id.to_string();
    let turn_end_policy = blocking
        .run_store("load RunTurn end policy", move |store| {
            let profile = store
                .get_run_state(&policy_run_id)?
                .map(|snapshot| snapshot.execution_profile)
                .ok_or_else(|| {
                    crate::tasks::task_runtime::StoreError::RunNotFound(policy_run_id.clone())
                })?;
            Ok(
                if profile.provenance
                    == crate::tasks::task_runtime::TaskRunProvenance::ConversationTurn
                {
                    crate::tasks::task_runtime::turn_lifecycle::TurnEndPolicy::ConversationTurn
                } else {
                    crate::tasks::task_runtime::turn_lifecycle::TurnEndPolicy::PolicyDriven
                },
            )
        })
        .await
        .map_err(|error| error.to_string())?;
    crate::tasks::task_runtime::turn_lifecycle::finalize_run_turn(
        &blocking,
        store,
        run_id,
        outcome,
        trace_sink,
        turn_end_policy,
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
    crate::tasks::task_runtime::TaskRuntimeOperation::new(store.clone())
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
    invocation_resources: Vec<echo_agent::tools::InvocationResourceGuard>,
    context_initialization: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

struct EkoTurnEventSink {
    state: std::sync::Arc<std::sync::Mutex<EkoTurnEventSinkState>>,
    sender: tokio::sync::mpsc::Sender<EkoTurnProjectionRequest>,
    _projector_task: tokio::task::JoinHandle<()>,
    domain_observation:
        Option<std::sync::Arc<crate::tasks::task_runtime::executor::EkoAgentTurnSink>>,
}

struct EkoTurnEventSinkState {
    webhook_observer: Option<WebhookTurnObserver>,
    expose_internal_synthesis: bool,
    downstream_failure: Option<echo_agent::error::AgentFailure>,
    progress_observer: Option<crate::turn_context::TurnProgressObserver>,
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
            progress_observer: None,
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
                if let Err(error) = crate::tasks::task_runtime::TaskRuntimeOperation::new(store)
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
            domain_observation: None,
        }
    }

    fn with_domain_observation(
        mut self,
        observation: Option<std::sync::Arc<crate::tasks::task_runtime::executor::EkoAgentTurnSink>>,
    ) -> Self {
        self.domain_observation = observation;
        self
    }

    fn with_progress_observer(self, observer: crate::turn_context::TurnProgressObserver) -> Self {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .progress_observer = Some(observer);
        self
    }

    fn finish(
        &self,
        receipt: TurnReceipt,
        framework_terminal: TurnOutcome,
    ) -> Result<TurnReceipt, ChatTurnError> {
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
        Ok(observer.finish(receipt, terminal))
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
        if let Some(observation) = self.domain_observation.as_ref() {
            observation.on_event(event.clone()).await?;
        }
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
        match crate::tasks::task_runtime::TaskRuntimeOperation::new(store.clone())
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
    // 内部续跑只扣住本轮最终综合；Framework已公开的阶段正文与独立Reasoning
    // 仍按原phase发布，否则60秒报告计时和失败后可见发现都会丢失。
    let suppress_internal_transcript = !state.expose_internal_synthesis
        && (matches!(&event.payload, AgentEvent::FinalAnswer(_))
            || matches!(&event.payload, AgentEvent::AssistantMessage { block }
                if block.phase == echo_agent::agent::AssistantPhase::FinalAnswer));
    if suppress_internal_transcript {
        return Ok(SinkControl::Continue);
    }
    let visible_report = match &event.payload {
        AgentEvent::AssistantMessage { block } => {
            // 公开资格由Framework发布路径保证；合法Unknown正文保持原phase，也应计作报告。
            matches!(
                block.phase,
                echo_agent::agent::AssistantPhase::Commentary
                    | echo_agent::agent::AssistantPhase::PartialAnswer
                    | echo_agent::agent::AssistantPhase::Unknown
            ) && !block.delta.trim().is_empty()
        }
        AgentEvent::FinalAnswer(text) => !text.trim().is_empty(),
        _ => false,
    };
    if sink.on_event(ChatDriverEvent::Agent(Box::new(event))) {
        if visible_report && let Some(observer) = state.progress_observer.as_ref() {
            observer.reported();
        }
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
) -> Result<TurnReceipt, ChatTurnError> {
    let ChatTurnModelScope {
        turn_id,
        bound_run_id,
        driver_execution_context,
        origin,
        transcript_visibility,
        invocation_resources,
        context_initialization,
    } = scope;
    // The turn preserves user-authored text (or a long-text artifact reference).
    let mut msg: Message = turn.to_message().map_err(|e| {
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
    let subagent_depth_binding = if let Some(run_id) = active_run_id.as_deref() {
        let store = runtime_store.as_ref().ok_or_else(|| {
            "A bound TaskRun requires its original runtime policy owner".to_string()
        })?;
        store
            .get_run_state(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("TaskRun '{run_id}' policy snapshot is missing"))?
            .execution_profile
            .subagent_depth_limit
    } else {
        Some(res.new_execution_subagent_depth().await)
    };
    let delegation_policy =
        crate::subagent_policy::primary_policy_for_binding(subagent_depth_binding);
    let domain_state = match (runtime_store.as_ref(), active_run_id.as_deref()) {
        (Some(store), Some(run_id)) => store
            .get_run_state(run_id)
            .map_err(|error| error.to_string())?,
        _ => None,
    };
    let domain_binding = domain_state
        .as_ref()
        .and_then(|state| state.domain_binding.as_ref());
    if let Some(binding) = domain_binding {
        let instructions = binding.prompt_instructions();
        match &mut msg.content {
            echo_agent::llm::types::MessageContent::Text(text) => {
                text.push_str("\n\n");
                text.push_str(&instructions);
            }
            echo_agent::llm::types::MessageContent::Parts(parts) => {
                parts.push(echo_agent::llm::types::ContentPart::Text { text: instructions })
            }
            echo_agent::llm::types::MessageContent::Empty => {
                msg.content = echo_agent::llm::types::MessageContent::Text(instructions)
            }
        }
    }
    let allowed_tools = domain_binding
        .as_ref()
        .map(|binding| binding.effective_allowed_tools(&[]))
        .transpose()?
        .flatten();
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
        let mut disabled = crate::tool_exposure::disabled_tools();
        if !delegation_policy.can_delegate() {
            disabled.insert("agent_tool".to_string());
        }
        let disabled_tools = Some(disabled);
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

        // Product history and receipts stay on this machine. A remote project
        // has its own execution namespace, already frozen by the turn scope;
        // retaining its local receipt must never change the tool cwd.
        let working_dir = Some(res.execution_scope.root().to_path_buf());
        let mut resource_guards = res
            .workspace_io_receipt
            .as_ref()
            .map(|receipt| receipt.invocation().resource_guards())
            .unwrap_or_default();

        resource_guards.extend(invocation_resources);

        let project_environment = res
            .store
            .as_ref()
            .and_then(|store| store.project_environment());
        let project_inputs = match project_environment {
            Some(environment) => Some((
                environment.files,
                environment.executor,
                Some(environment.state_dir),
            )),
            None => match guard.file_system() {
                Some(files) => Some((
                    files,
                    guard.sandbox_manager().cloned().ok_or_else(|| {
                        ChatTurnError::from(
                            "project invocation executor is unavailable".to_string(),
                        )
                    })?,
                    res.workspace_io_receipt.as_ref().map(|receipt| {
                        crate::workspace::layout::WorkspaceLayout::state_dir(receipt.data_root())
                    }),
                )),
                None if res.workspace_io_receipt.is_some() => {
                    return Err(ChatTurnError::from(
                        "registered project invocation file environment is unavailable".to_string(),
                    ));
                }
                // Minimal embeddings with neither a Workspace receipt nor an
                // explicit provider retain their original context behavior.
                None => None,
            },
        };
        if let Some((files, executor, local_state_dir)) = project_inputs {
            let preparation_started = std::time::Instant::now();
            let prepared = crate::infra::prepare_dynamic_project_context(
                working_dir.as_deref(),
                local_state_dir.as_deref(),
                files,
                executor,
                resource_guards.clone(),
                guard.config().get_token_limit(),
            )
            .await
            .map_err(ChatTurnError::from)?;
            // 先成功发布当前事实，再向原pool缓存确认初始化；活动steer和只读
            // 面板可继续复用同一实例。Execute/reset 后仍由原turn projector重发。
            guard.context().lock().await.apply_projections(&prepared.projections());
            if let Some(initialized) = context_initialization.as_ref() {
                initialized.store(true, std::sync::atomic::Ordering::Release);
            }
            tracing::info!(%turn_id, elapsed_ms = preparation_started.elapsed().as_millis(), "turn project context prepared");
            _turn_projection_registration
                .install_project_context(prepared)
                .map_err(ChatTurnError::from)?;
        }

        // `with_run_context` is task-local and does not cross the framework's
        // forked subagent `tokio::spawn`; ExternalRunContext is the value-carried
        // channel that keeps Subagent tools and run_id on this same run. The
        // `trace_sink` here is the framework-Value form; `scoped_with_ctx_run_id`
        // re-scopes it into `CURRENT_TRACE_SINK` for tools (e.g. task_execute)
        // running inside the framework's spawned tool executor.
        if let Some(source_conversation) = res.conv_id.as_ref() {
            resource_guards.push(echo_agent::tools::InvocationResourceGuard::new_identified(
                (),
                crate::agent_control::ConversationTarget {
                    workspace_id: res.execution_scope.workspace_id().to_string(),
                    conversation_id: source_conversation.clone(),
                    workspace_generation: None,
                },
            ));
        }
        // 当前有限turn与本surface已经选定的journal根分别携带；恢复不能改写根身份。
        let event_identity = EventIdentity::for_chat(
            conversation_id.clone(),
            turn_id.clone(),
            res.root_message_id.clone(),
            active_run_id.clone(),
        )
        .map_err(|error| error.to_string())?;
        // 传原Foreground物理owner给原事件头；历史确认不应从文字或Run重建执行身份。
        let event_identity = match driver_execution_context.as_deref() {
            Some(execution_id) => event_identity
                .with_execution_id(execution_id)
                .map_err(|error| error.to_string())?,
            None => event_identity,
        };
        let invocation = echo_agent::agent::AgentInvocationContext {
            allowed_tools,
            tool_access: None,
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
                delegation_policy: Some(delegation_policy),
                resource_guards: Vec::new(),
                subagent_lineage: None,
                uplink: None,
            }),
            working_dir,
            cancel: None,
            disabled_tools,
            visible_tools,
            run_budget: None,
            resource_guards,
        };
        let domain_observation = domain_state
            .as_ref()
            .filter(|state| state.domain_binding.is_some())
            .map(|state| {
                let mutating_tools = guard
                    .tool_names()
                    .into_iter()
                    .filter(|name| {
                        crate::tasks::task_runtime::executor::tool_call_may_mutate_workspace(
                            &guard, name,
                        )
                    })
                    .collect();
                std::sync::Arc::new(
                    crate::tasks::task_runtime::executor::EkoAgentTurnSink::observe_chat(
                        &state.run,
                        mutating_tools,
                    ),
                )
            });
        let eko_sink = EkoTurnEventSink::new(
            sink,
            webhook_observer,
            active_run_id.clone(),
            runtime_store.clone(),
            turn_id.clone(),
            transcript_visibility,
        )
        .with_domain_observation(domain_observation.clone())
        .with_progress_observer(_turn_projection_registration.progress_observer());
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
        let mut receipt = receipt.map_err(ChatTurnError::from)?;
        receipt.outcome =
            effective_terminal_after_input_observer(receipt.outcome.clone(), observer_result);
        if let (Some(observation), Some(store), Some(run_id)) =
            (domain_observation, runtime_store, active_run_id)
        {
            let observation = observation.finish(receipt.final_answer.as_deref());
            crate::tasks::task_runtime::executor::finalize_direct_agent_observation(
                &store,
                &run_id,
                &mut receipt,
                observation,
                &res.cancel,
            )
            .await
            .map_err(|error| error.to_string())?;
        }
        let terminal = receipt.outcome.clone();
        eko_sink.finish(receipt, terminal)
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

    #[test]
    fn foreground_projection_keeps_deferred_paused_cancelled_and_foreign_runs_non_successful()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::tasks::task_runtime::continuation::{
            ContinuationCompletion, ContinuationCompletionReason,
        };
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, TaskRunExecutionProfile, TaskRuntimeStore,
        };
        use std::sync::Arc;
        let store = TaskRuntimeStore::new_in_memory()?;
        let workspace = store.active_workspace_id();
        let mut run = store.create_run_with_profile(
            "projection-run",
            &workspace,
            "projection-conversation",
            "formal-root",
            DomainProfile::General,
            "原Goal",
            "agent_task_plan",
            AttendedMode::Attended,
            TaskRunExecutionProfile::conversation_turn(),
        )?;
        // 此处只测原Run读模型的呈现；真正Completed提交及物理释放由r06真实入口验证。
        run.status = TaskRunStatus::Completed;
        let binding = RunTurnBinding {
            domain_plugin: None,
            run_id: Some(run.run_id.clone()),
            turn_id: "current-finite-turn".into(),
            root_message_id: "formal-root".into(),
            origin: RunTurnOrigin::Continuation,
            transcript_visibility: TurnVisibility::Internal,
            expected_resume: None,
        };
        let resources = crate::chat_resources::ChatResources {
            execution_scope: crate::workspace::WorkspaceExecutionScope::workspace(
                &crate::workspace::WorkspaceId::from_name(&workspace),
                ".",
            ),
            workspace_io_receipt: None,
            pool: None,
            store: None,
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some(run.conversation_id.clone()),
            root_message_id: "surface-resume-message".into(),
            attachments: Vec::new(),
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        };
        let completion = ContinuationCompletion {
            terminal: TurnOutcome::Completed,
            reason: ContinuationCompletionReason::Stopped,
        };
        assert_eq!(
            foreground_continuation_projection(
                &completion,
                &run,
                &run.run_id,
                &binding,
                &resources
            )?,
            Some(TurnOutcome::Completed)
        );
        for status in [
            TaskRunStatus::Pending,
            TaskRunStatus::Running,
            TaskRunStatus::Paused,
            TaskRunStatus::Failed,
            TaskRunStatus::Cancelled,
        ] {
            let mut observed = run.clone();
            observed.status = status;
            assert!(
                foreground_continuation_projection(
                    &completion,
                    &observed,
                    &run.run_id,
                    &binding,
                    &resources
                )?
                .is_none()
            );
        }
        let deferred = ContinuationCompletion {
            terminal: TurnOutcome::Completed,
            reason: ContinuationCompletionReason::Deferred,
        };
        assert!(
            foreground_continuation_projection(&deferred, &run, &run.run_id, &binding, &resources)?
                .is_none()
        );
        let cancelled = ContinuationCompletion {
            terminal: TurnOutcome::Cancelled,
            reason: ContinuationCompletionReason::Stopped,
        };
        assert!(
            foreground_continuation_projection(
                &cancelled,
                &run,
                &run.run_id,
                &binding,
                &resources
            )?
            .is_none()
        );
        for field in ["run", "workspace", "conversation", "root"] {
            let mut observed = run.clone();
            match field {
                "run" => observed.run_id = "foreign-run".into(),
                "workspace" => observed.workspace_id = "foreign-workspace".into(),
                "conversation" => observed.conversation_id = "foreign-conversation".into(),
                _ => observed.root_message_id = "foreign-root".into(),
            }
            assert!(
                foreground_continuation_projection(
                    &completion,
                    &observed,
                    &run.run_id,
                    &binding,
                    &resources
                )
                .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn existing_run_continuation_preserves_identity_and_rejects_foreign_bindings()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, RunPlanPolicy, TaskRunExecutionProfile, TaskRuntimeStore,
        };
        let directory = tempfile::tempdir()?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(directory.path())?;
        let workspace = store.active_workspace_id();
        let original = store.create_run_with_profile(
            "original-unattended",
            &workspace,
            "original-conversation",
            "original-root",
            DomainProfile::DataAnalysis,
            "original user Goal",
            "agent_autonomous",
            AttendedMode::Unattended,
            TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
        )?;
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<ExecEvent>::new()));
        let sink_events = events.clone();
        let trace_sink: crate::tasks::task_runtime::task_tools::TraceSink =
            std::sync::Arc::new(move |event| {
                if let Ok(mut events) = sink_events.lock() {
                    events.push(event);
                }
            });
        ensure_bound_task_run(
            &store,
            &original.run_id,
            Some("original-conversation"),
            "original-root",
            "internal continuation instruction",
            &[],
            Some(&trace_sink),
        )?;
        let actual = store
            .get_run(&original.run_id)?
            .ok_or("original run disappeared")?;
        assert_eq!(actual.domain_profile, original.domain_profile);
        assert_eq!(actual.route, original.route);
        assert_eq!(actual.attended_mode, original.attended_mode);
        assert_eq!(actual.goal, original.goal);
        let traces = events.lock().map_err(|_| "trace lock poisoned")?.clone();
        assert_eq!(traces.len(), 1);
        let started = traces.first().ok_or("original run-start event missing")?;
        assert_eq!(started.event, RuntimeEventKind::RunStarted);
        assert_eq!(
            started
                .payload
                .get("goal")
                .and_then(serde_json::Value::as_str),
            Some("original user Goal")
        );
        assert_eq!(
            started
                .payload
                .get("route")
                .and_then(serde_json::Value::as_str),
            Some("agent_autonomous")
        );
        let before = serde_json::to_value(store.list_events(&original.run_id, 0)?)?;
        for (conversation, root) in [
            ("wrong-conversation", "original-root"),
            ("original-conversation", "wrong-root"),
        ] {
            assert!(
                ensure_bound_task_run(
                    &store,
                    &original.run_id,
                    Some(conversation),
                    root,
                    "internal continuation instruction",
                    &[],
                    None
                )
                .is_err()
            );
        }
        assert_eq!(
            serde_json::to_value(store.list_events(&original.run_id, 0)?)?,
            before
        );
        store.create_run_with_profile(
            "foreign-workspace",
            "another-workspace",
            "original-conversation",
            "original-root",
            DomainProfile::DataAnalysis,
            "foreign Goal",
            "agent_autonomous",
            AttendedMode::Unattended,
            TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
        )?;
        let foreign_before = serde_json::to_value(store.list_events("foreign-workspace", 0)?)?;
        assert!(
            ensure_bound_task_run(
                &store,
                "foreign-workspace",
                Some("original-conversation"),
                "original-root",
                "internal continuation instruction",
                &[],
                None
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(store.list_events("foreign-workspace", 0)?)?,
            foreign_before
        );
        Ok(())
    }

    #[test]
    fn product_event_wire_contract_preserves_canonical_identity_and_payloads() -> Result<(), String>
    {
        let identity = EventIdentity::new("stream-1", "turn-1")
            .map_err(|error| error.to_string())?
            .with_conversation_id("conversation-1")
            .map_err(|error| error.to_string())?
            .with_run_id("run-1")
            .map_err(|error| error.to_string())?
            .with_message_id("message-1")
            .map_err(|error| error.to_string())?
            .with_execution_id("execution-1")
            .map_err(|error| error.to_string())?
            .with_parent_event_id("parent-1")
            .map_err(|error| error.to_string())?;
        let events = [
            ChatDriverEvent::Agent(Box::new(
                EventEnvelope::new(
                    &identity,
                    7,
                    identity.parent_event_id.clone(),
                    AgentEvent::ToolResult {
                        call_id: "call-1".to_string(),
                        name: "shell".to_string(),
                        result: echo_agent::tools::ToolResult::failure(
                            echo_agent::tools::ToolFailureCategory::Timeout,
                            "timed out",
                        ),
                    },
                )
                .map_err(|error| error.to_string())?,
            )),
            ChatDriverEvent::Execution(Box::new(ExecEvent::subagent(
                "workspace-1",
                "conversation-1",
                "run-1",
                "task-1",
                "execution-1",
                RuntimeEventKind::ArtifactProduced,
                serde_json::json!({
                    "execution_id": "execution-1",
                    "artifact": {
                        "path": "/tmp/full.log",
                        "bytes": 1234,
                        "sha256": "abc123",
                        "retention": "run",
                        "available": true
                    }
                }),
            ))),
            ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            },
            ChatDriverEvent::ExecutionPath {
                observed_path: "task_runtime".to_string(),
            },
            ChatDriverEvent::Interrupt {
                run_id: "run-1".to_string(),
                goal: "revise".to_string(),
                new_message: "use the file store".to_string(),
            },
        ];
        let values = events
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let sources = values
            .iter()
            .filter_map(|value| value.get("source"))
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            sources,
            [
                "agent",
                "execution",
                "turn_status",
                "execution_path",
                "interrupt"
            ]
        );

        let agent = values
            .first()
            .ok_or_else(|| "agent event missing".to_string())?;
        for (pointer, expected) in [
            ("/event/stream_id", "stream-1"),
            ("/event/run_id", "run-1"),
            ("/event/turn_id", "turn-1"),
            ("/event/execution_id", "execution-1"),
            ("/event/parent_event_id", "parent-1"),
            ("/event/payload/type", "tool_result"),
            ("/event/payload/data/result/error", "timed out"),
        ] {
            assert_eq!(
                agent.pointer(pointer).and_then(serde_json::Value::as_str),
                Some(expected)
            );
        }

        let execution = values
            .get(1)
            .ok_or_else(|| "execution event missing".to_string())?;
        for (pointer, expected) in [
            ("/event/run_id", "run-1"),
            ("/event/task_id", "task-1"),
            ("/event/payload/artifact/path", "/tmp/full.log"),
            ("/event/payload/artifact/sha256", "abc123"),
        ] {
            assert_eq!(
                execution
                    .pointer(pointer)
                    .and_then(serde_json::Value::as_str),
                Some(expected)
            );
        }
        Ok(())
    }

    fn test_execution_scope() -> crate::workspace::WorkspaceExecutionScope {
        crate::workspace::WorkspaceExecutionScope::workspace(
            &crate::workspace::WorkspaceId::from_name("test"),
            ".",
        )
    }

    // Controlled native input keeps the original transport assertions independent
    // of provider phase inference and runtime terminal confirmation.
    fn native_progress_fixture(text: impl Into<String>, sequence: u64) -> AgentEvent {
        AgentEvent::AssistantMessage {
            block: echo_agent::agent::AssistantMessageBlock {
                provider_source: None,
                block_id: format!("eko-native-fixture-{sequence}"),
                request_id: "eko-native-fixture-request".to_string(),
                sequence,
                phase: echo_agent::agent::AssistantPhase::Commentary,
                delta: text.into(),
                complete: false,
                provider: Some("eko-native-test-fixture".to_string()),
                reasoning: None,
            },
        }
    }

    /// Build a minimal [`PreparedUserTurn`] for tests that do not exercise
    /// spill or attachment logic.
    fn make_turn(text: &str) -> crate::prepared_turn::PreparedUserTurn {
        crate::prepared_turn::PreparedUserTurn {
            instruction: text.to_string(),
            resources: vec![],
            authorship: crate::prepared_turn::InstructionAuthorship::User,
            agent_input: None,
        }
    }

    #[tokio::test]
    async fn original_input_source_reaches_real_driver_and_full_goal_after_spill()
    -> anyhow::Result<()> {
        use crate::chat_event_log::{ChatEventLog, ChatEventRetention};
        use crate::conversation_input::{
            ConversationInputAddress, ConversationInputService, ConversationInputSource,
        };
        use crate::tasks::task_runtime::{RunInputSnapshot, RuntimeEventKind, TaskRuntimeStore};
        use std::sync::Arc;
        let directory = tempfile::tempdir()?;
        // 与真实工作区共用 host artifact 根，保持长 Goal 与输入 spill 的读取合同。
        let task_root = crate::workspace::layout::WorkspaceLayout::tasks(directory.path());
        let store = Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            &task_root,
        )?);
        let _resource_scope = store.test_resource_scope();
        let log = Arc::new(ChatEventLog::open(
            directory.path().join("chat"),
            ChatEventRetention::default(),
        )?);
        let service = ConversationInputService::new(log);
        let address = ConversationInputAddress {
            workspace_id: store.active_workspace_id(),
            conversation_id: "original-input".into(),
        };
        let text = format!("{}完整原始Goal尾部🧭", "原文来源 ".repeat(3000));
        let saved = service
            .submit(
                address.clone(),
                "original-input-id".into(),
                text.clone(),
                Vec::new(),
            )
            .await?;
        let turn_id = "original-input-turn";
        let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(turn_id);
        let started = service
            .dispatch_selected(saved.identity, saved.queue_revision, turn_id.into())
            .await?;
        let attempt = started
            .active_attempt
            .clone()
            .ok_or_else(|| anyhow::anyhow!("真实attempt缺失"))?;
        service
            .bind_task_target(attempt.clone(), run_id.clone())
            .await?;
        let source = crate::conversation_input::BoundRunInput {
            target: crate::conversation_input::ConversationInputTaskTarget {
                run_id: run_id.clone(),
                turn_id: turn_id.into(),
                identity: started.receipt.identity.clone(),
            },
            snapshot: RunInputSnapshot {
                source: ConversationInputSource::Gui,
                identity: started.receipt.identity.clone(),
                payload: started.payload.clone(),
            },
        };
        let prepared =
            crate::prepared_turn::PreparedUserTurn::build(crate::prepared_turn::UserTurnInput {
                text: &text,
                attachments: &[],
                spill_dir: &crate::workspace::layout::WorkspaceLayout::user_input_artifacts(
                    directory.path(),
                ),
                conversation_id: Some(&address.conversation_id),
                turn_id: Some(turn_id),
            })?;
        assert_ne!(prepared.instruction, text);
        assert!(!prepared.resources.is_empty());
        let llm = Arc::new(echo_agent::testing::MockLlmClient::new().with_response("完成"));
        let react_agent = echo_agent::agent::ReactAgentBuilder::new()
            .model("source-admission")
            .llm_client(llm.clone())
            .tool_output_artifacts(crate::infra::tool_output_artifact_config(Some(
                directory.path(),
            )))
            .tool(Box::new(
                echo_agent::tools::files::files::ReadFileTool::new(),
            ))
            .tool(Box::new(
                echo_agent::tools::files::artifact::ReadArtifactTool,
            ))
            .build()?;
        react_agent.set_pre_model_context_projector(Some(Arc::new(
            crate::turn_context::EkoContextProjector::new(
                crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
                crate::turn_context::turn_prompt_context_registry(),
            ),
        )));
        let agent = AgentHandle::new(react_agent);
        let sink = Arc::new(MockChatSink::default());
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: sink.clone(),
            webhook_emitter: None,
            conv_id: Some(address.conversation_id.clone()),
            root_message_id: turn_id.into(),
            attachments: prepared.inline_attachment_refs(),
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let observe_service = service.clone();
        let observe_attempt = attempt.clone();
        let observer: InputReceiptObserver = Arc::new(move |receipt| {
            let service = observe_service.clone();
            let attempt = observe_attempt.clone();
            Box::pin(async move {
                service
                    .observe_turn_input_through_drain(attempt, receipt)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        });
        let driven = drive_chat_turn_with_input_source(
            &agent,
            &prepared,
            resources,
            None,
            Some(observer),
            Some(source),
        )
        .await;
        let idle = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.wait_for_run_driver_idle(&run_id),
        )
        .await;
        let outcome = driven.map_err(|error| anyhow::anyhow!(error.to_string()))?;
        idle?;
        service.settle_attempt(&attempt, &outcome.outcome).await?;
        assert!(
            matches!(outcome.outcome, TurnOutcome::Completed),
            "original driver receipt={outcome:?}; model_calls={}; native_events={:?}",
            llm.call_count(),
            sink.events
                .lock()
                .map_err(|_| anyhow::anyhow!("original native event record lock poisoned"))?
                .as_slice(),
        );
        assert!(llm.call_count() > 0);
        let events = store.list_events(&run_id, 0)?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::RunInputAccepted)
                .count(),
            1
        );
        assert_eq!(
            store
                .get_run(&run_id)?
                .ok_or_else(|| anyhow::anyhow!("原任务缺失"))?
                .goal,
            text
        );
        drop(agent);
        drop(store);
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&task_root)?;
        assert_eq!(
            reopened
                .get_run(&run_id)?
                .ok_or_else(|| anyhow::anyhow!("冷重开原任务缺失"))?
                .goal,
            text
        );
        assert_eq!(service.list(&address).await?.items.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_original_input_blocks_real_driver_before_model_and_preserves_queue()
    -> anyhow::Result<()> {
        use crate::chat_event_log::{ChatEventLog, ChatEventRetention};
        use crate::conversation_input::{
            ConversationInputAddress, ConversationInputPhase, ConversationInputService,
            ConversationInputSource,
        };
        use crate::tasks::task_runtime::{RunInputSnapshot, TaskRuntimeStore};
        use std::sync::Arc;
        let directory = tempfile::tempdir()?;
        let store = Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            directory.path().join("tasks"),
        )?);
        let _resource_scope = store.test_resource_scope();
        let service = ConversationInputService::new(Arc::new(ChatEventLog::open(
            directory.path().join("chat"),
            ChatEventRetention::default(),
        )?));
        let address = ConversationInputAddress {
            workspace_id: store.active_workspace_id(),
            conversation_id: "invalid-source".into(),
        };
        let saved = service
            .submit(
                address.clone(),
                "invalid-source-id".into(),
                "用户原输入保留".into(),
                Vec::new(),
            )
            .await?;
        let turn_id = "invalid-source-turn";
        let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(turn_id);
        let started = service
            .dispatch_selected(saved.identity, saved.queue_revision, turn_id.into())
            .await?;
        let attempt = started
            .active_attempt
            .clone()
            .ok_or_else(|| anyhow::anyhow!("真实attempt缺失"))?;
        service
            .bind_task_target(attempt.clone(), run_id.clone())
            .await?;
        let mut source = crate::conversation_input::BoundRunInput {
            target: crate::conversation_input::ConversationInputTaskTarget {
                run_id: run_id.clone(),
                turn_id: turn_id.into(),
                identity: started.receipt.identity.clone(),
            },
            snapshot: RunInputSnapshot {
                source: ConversationInputSource::Gui,
                identity: started.receipt.identity,
                payload: started.payload,
            },
        };
        source.snapshot.payload.text = "伪造的其它正文".into();
        let llm = Arc::new(echo_agent::testing::MockLlmClient::new().with_response("不应调用"));
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("invalid-source")
                .llm_client(llm.clone())
                .build()?,
        );
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some(address.conversation_id.clone()),
            root_message_id: turn_id.into(),
            attachments: Vec::new(),
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let result = drive_chat_turn_with_input_source(
            &agent,
            &make_turn("引用预览"),
            resources,
            None,
            None,
            Some(source),
        )
        .await;
        service
            .recovery_required(attempt, "输入来源校验失败，未执行".into())
            .await?;
        let retained = service.list(&address).await?;
        assert!(result.is_err());
        assert_eq!(llm.call_count(), 0);
        assert!(store.get_run(&run_id)?.is_none());
        let item = retained
            .items
            .first()
            .ok_or_else(|| anyhow::anyhow!("原队列丢失"))?;
        assert_eq!(item.payload.text, "用户原输入保留");
        assert_eq!(item.receipt.phase, ConversationInputPhase::RecoveryRequired);
        assert!(!item.receipt.drained);
        assert_eq!(
            item.task_target
                .as_ref()
                .map(|target| target.run_id.as_str()),
            Some(run_id.as_str())
        );
        Ok(())
    }

    #[tokio::test]
    async fn shared_foreground_reopened_owner_intent_stays_nonterminal() -> anyhow::Result<()> {
        use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, TaskRunResumeIdentity, TaskRunStatus, TaskRuntimeStore,
        };
        use echo_agent::state::journal::{EventJournal, FileEventJournal};
        use echo_agent::utils::fs::FileDurability;
        use std::sync::Arc;

        for surface in [
            ForegroundTurnSurface::Gui,
            ForegroundTurnSurface::Tui,
            ForegroundTurnSurface::Cli,
            ForegroundTurnSurface::Channel,
        ] {
            let directory = tempfile::tempdir()?;
            let store = TaskRuntimeStore::new_in_memory_with_shadow_root(directory.path())?;
            store.create_run(
                "shared-owner",
                "test",
                "shared-conversation",
                "original-root",
                DomainProfile::General,
                "preserve original owner",
                "agent_task_plan",
                AttendedMode::Attended,
            )?;
            store.transition_run("shared-owner", TaskRunStatus::Running)?;
            store.transition_run("shared-owner", TaskRunStatus::Paused)?;
            drop(store);
            let journal_path = directory.path().join("shared-owner/events.jsonl");
            // Official durable original-owner intent followed by a fresh Store;
            // no live driver or helper-injected observation masks admission.
            let journal = FileEventJournal::<serde_json::Value>::open(
                &journal_path,
                FileDurability::SyncData,
            )?;
            journal
                .append(serde_json::json!({
                    "run_id":"shared-owner", "task_id":null, "step_id":"original-execution",
                    "event_type":"recovery_blocked", "timestamp":chrono::Utc::now(),
                    "payload":{"physical_owner_intent":{
                        "run_id":"shared-owner", "owner_id":"original-owner",
                        "execution_id":"original-execution", "workspace_id":"test",
                        "physical_terminal_observed":false
                    }}
                }))
                .map_err(|error| anyhow::anyhow!("persist original intent: {error:?}"))?;
            drop(journal);
            let store = Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
                directory.path(),
            )?);
            let original = store
                .get_run_state("shared-owner")?
                .ok_or_else(|| anyhow::anyhow!("original run absent"))?;
            let before_journal = std::fs::read(&journal_path)?;
            let before_state = serde_json::to_value(&original)?;
            let llm = Arc::new(
                echo_agent::testing::MockLlmClient::new()
                    .with_model_name("shared-owner-test")
                    .with_response("must not execute"),
            );
            let agent = AgentHandle::new(
                echo_agent::agent::ReactAgentBuilder::new()
                    .model("shared-owner-test")
                    .llm_client(llm.clone())
                    .build()?,
            );
            let sink = Arc::new(MockChatSink::default());
            let control = ForegroundTurnControl::default();
            let lease =
                control.begin_scoped("test", surface, "shared-conversation", "resume-turn")?;
            let resources = Arc::new(crate::chat_resources::ChatResources {
                execution_scope: test_execution_scope(),
                workspace_io_receipt: None,
                pool: None,
                store: Some(store.clone()),
                sink: sink.clone(),
                webhook_emitter: None,
                conv_id: Some("shared-conversation".to_string()),
                root_message_id: "resume-turn".to_string(),
                attachments: Vec::new(),
                cancel: lease.cancellation_token(),
                review_integration: None,
                memory_generation: None,
                human_loop_provider: None,
            });
            let result = crate::foreground_turn::drive_foreground_chat_turn(
                lease,
                &agent,
                &make_turn("resume original"),
                resources,
                RunTurnBinding::resume_expected(
                    TaskRunResumeIdentity::capture(&original),
                    "resume-turn",
                ),
            )
            .await;
            assert!(
                matches!(&result, Err(ChatTurnError::Observation { run_id, execution_id: None })
                            if run_id == "shared-owner"),
                "shared {surface} preparation projected durable unknown ownership as terminal: {result:?}"
            );
            assert!(
                control
                    .snapshot_scoped("test", surface, "shared-conversation")
                    .is_some()
            );
            assert_eq!(llm.call_count(), 0);
            assert_eq!(sink.event_count(), 0);
            assert_eq!(std::fs::read(&journal_path)?, before_journal);
            assert_eq!(
                serde_json::to_value(
                    store
                        .get_run_state("shared-owner")?
                        .ok_or_else(|| anyhow::anyhow!("original run disappeared"))?
                )?,
                before_state
            );
            assert!(!store.is_run_active("shared-owner"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn shared_foreground_ordinary_resume_claim_error_still_settles_failed()
    -> anyhow::Result<()> {
        use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, TaskRunResumeIdentity, TaskRunStatus, TaskRuntimeStore,
        };
        use std::sync::Arc;

        let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
        let _original_resource_scope = store.test_resource_scope();
        store.create_run(
            "ordinary-resume",
            "test",
            "ordinary-conversation",
            "original-root",
            DomainProfile::General,
            "preserve ordinary identity",
            "agent_task_plan",
            AttendedMode::Attended,
        )?;
        store.transition_run("ordinary-resume", TaskRunStatus::Running)?;
        store.configure_run_continuation("ordinary-resume", true, false, None, None)?;
        store.transition_run("ordinary-resume", TaskRunStatus::Paused)?;
        let original = store
            .get_run_state("ordinary-resume")?
            .ok_or_else(|| anyhow::anyhow!("ordinary run absent"))?;
        let mut expected = TaskRunResumeIdentity::capture(&original);
        expected.goal_revision = expected.goal_revision.saturating_add(1);
        let llm =
            Arc::new(echo_agent::testing::MockLlmClient::new().with_model_name("ordinary-test"));
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("ordinary-test")
                .llm_client(llm.clone())
                .build()?,
        );
        let control = ForegroundTurnControl::default();
        let lease = control.begin_scoped(
            "test",
            ForegroundTurnSurface::Tui,
            "ordinary-conversation",
            "resume-turn",
        )?;
        let waiter = control.settlement_waiter_scoped(
            "test",
            ForegroundTurnSurface::Tui,
            "ordinary-conversation",
            "resume-turn",
        )?;
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("ordinary-conversation".to_string()),
            root_message_id: "resume-turn".to_string(),
            attachments: Vec::new(),
            cancel: lease.cancellation_token(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let result = crate::foreground_turn::drive_foreground_chat_turn(
            lease,
            &agent,
            &make_turn("resume ordinary"),
            resources,
            RunTurnBinding::resume_expected(expected, "resume-turn"),
        )
        .await?;
        assert!(
            matches!(&result, TurnOutcome::Failed(error) if error.message.contains("identity changed"))
        );
        assert!(matches!(
            waiter.wait().await?.outcome,
            TurnOutcome::Failed(_)
        ));
        assert!(
            control
                .snapshot_scoped("test", ForegroundTurnSurface::Tui, "ordinary-conversation")
                .is_none()
        );
        assert_eq!(llm.call_count(), 0);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.wait_for_run_driver_idle("ordinary-resume"),
        )
        .await?;
        assert!(!store.is_run_active("ordinary-resume"));
        Ok(())
    }

    fn test_turn_receipt(turn_id: &str, outcome: TurnOutcome) -> Result<TurnReceipt, String> {
        let identity = EventIdentity::new(format!("test-stream-{turn_id}"), turn_id)
            .map_err(|error| error.to_string())?;
        Ok(TurnReceipt {
            turn_id: identity.turn_id,
            outcome,
            delivery: echo_agent::runtime::TurnDeliveryOutcome::NotAttempted,
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

    #[tokio::test]
    async fn completed_first_turn_retires_continuation_renderer() -> Result<(), String> {
        let turn_id = "first-turn-renderer-release";
        let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(turn_id);
        let store = std::sync::Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let _resource_scope = store.test_resource_scope();
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("first-turn-renderer-release")
                .llm_client(std::sync::Arc::new(
                    echo_agent::testing::MockLlmClient::new().with_response("完成"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let sink = std::sync::Arc::new(MockChatSink::default());
        let renderer = std::sync::Arc::downgrade(&sink);
        let resources = std::sync::Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: sink.clone(),
            webhook_emitter: None,
            conv_id: Some("first-turn-renderer-conversation".to_string()),
            root_message_id: turn_id.to_string(),
            attachments: Vec::new(),
            cancel: echo_agent::agent::CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let outcome = drive_chat(&agent, &make_turn("完成当前普通回合"), resources)
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, TurnOutcome::Completed));
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.wait_for_run_driver_idle(&run_id),
        )
        .await
        .map_err(|error| error.to_string())?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "原 TaskRun 缺失".to_string())?;
        assert_eq!(
            run.status,
            crate::tasks::task_runtime::TaskRunStatus::Completed
        );
        let (launcher, active, pending) =
            crate::tasks::task_runtime::continuation::runtime_state_for_test(&store, &run_id);
        drop(sink);
        if launcher.is_some() || active.is_some() || pending || renderer.upgrade().is_some() {
            return Err(format!(
                "首回合已完成但仍持有 renderer：launcher={launcher:?}, active={active:?}, pending={pending}, renderer_strong={}",
                renderer.strong_count()
            ));
        }
        Ok(())
    }

    async fn drive_successful_model_with_observer(
        root_message_id: &str,
        observer_result: Result<(), String>,
    ) -> Result<(TurnReceipt, std::sync::Arc<MockChatSink>), String> {
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
        .await
        .map_err(|error| error.to_string())?;
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
                domain_binding: None,

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
        let outcome =
            TurnReceipt::failed("provider-turn", failure).map_err(|error| error.to_string())?;
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
            memory_generation: None,
            human_loop_provider: None,
        });
        let outcome = drive_chat_inner(
            &agent,
            &make_turn("continue"),
            resources,
            ChatTurnModelScope {
                invocation_resources: Vec::new(),
                context_initialization: None,
                turn_id: "setup-turn".to_string(),
                bound_run_id: Some("setup-retry".to_string()),
                driver_execution_context: None,
                origin: crate::tasks::task_runtime::RunTurnOrigin::Continuation,
                transcript_visibility: crate::tasks::task_runtime::TurnVisibility::Internal,
            },
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        let TurnOutcome::Failed(failure) = &outcome.outcome else {
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
    async fn continuation_journal_keeps_original_root_across_two_finite_turns() -> Result<(), String>
    {
        use crate::chat_event_log::{
            ChatEventLog, ChatEventRetention, ChatSurface, bind_surface_chat_sink,
        };
        use std::sync::Arc;
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(
                directory.path().join("chat-events"),
                ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        let renderer = Arc::new(MockChatSink::default());
        let sink = bind_surface_chat_sink(
            ChatSurface::Channel,
            renderer.clone(),
            log.clone(),
            Arc::new(
                crate::tool_execution::ToolExecutionRepository::open(
                    directory.path().join("tools"),
                )
                .map_err(|error| error.to_string())?,
            ),
            "history-workspace",
            Some("history-conversation".to_string()),
            "original-root",
        );
        let model = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_network_error("retryable local transport failure")
                .with_response("恢复后的回答"),
        );
        let mut raw = echo_agent::agent::ReactAgent::new(
            echo_agent::agent::AgentConfig::minimal("root-history", "root-history")
                .llm_max_retries(0),
        );
        raw.set_llm_client(model.clone());
        let agent = AgentHandle::new(raw);
        let cancel = echo_agent::agent::CancellationToken::new();
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: None,
            sink,
            webhook_emitter: None,
            conv_id: Some("history-conversation".to_string()),
            root_message_id: "original-root".to_string(),
            attachments: Vec::new(),
            cancel: cancel.clone(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        for (turn_id, origin) in [
            ("original-root", RunTurnOrigin::User),
            ("continuation-turn", RunTurnOrigin::Continuation),
        ] {
            let receipt = drive_chat_inner(
                &agent,
                &make_turn("继续同一任务"),
                resources.clone(),
                ChatTurnModelScope {
                    turn_id: turn_id.to_string(),
                    bound_run_id: None,
                    driver_execution_context: Some(format!("execution-{turn_id}")),
                    origin,
                    transcript_visibility: TurnVisibility::Internal,
                    invocation_resources: Vec::new(),
                    context_initialization: None,
                },
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
            if turn_id == "original-root" {
                let TurnOutcome::Failed(failure) = receipt.outcome else {
                    return Err("首回合未保留原网络失败".to_string());
                };
                assert_eq!(failure.code, "llm_network");
                assert!(failure.retryable);
            } else {
                assert_eq!(
                    receipt.outcome,
                    TurnOutcome::Completed,
                    "续接不能因原journal根身份错误变成断流/取消"
                );
            }
        }
        let replay = log
            .replay(
                "history-workspace",
                Some("history-conversation"),
                "original-root",
                0,
            )
            .map_err(|error| error.to_string())?;
        assert!(
            replay
                .events
                .iter()
                .any(|event| event.turn_id == "original-root")
        );
        assert!(
            replay
                .events
                .iter()
                .any(|event| event.turn_id == "continuation-turn")
        );
        for event in &replay.events {
            assert_eq!(event.root_turn_id, "original-root");
            assert_eq!(event.message_id, "original-root");
            if let ChatDriverEvent::Agent(inner) = &event.payload {
                assert_eq!(
                    inner.message_id.as_ref().map(|id| id.as_str()),
                    Some("original-root")
                );
                assert!(
                    !matches!(&inner.payload, AgentEvent::ExecutionStatus(status)
                    if status.status == echo_agent::agent::ExecutionStatusKind::Cancelled)
                );
            }
        }
        assert_eq!(model.call_count(), 2);
        assert!(!cancel.is_cancelled());
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
        let outcome = TurnReceipt::failed("io-turn", failure).map_err(|error| error.to_string())?;
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
        let outcome =
            TurnReceipt::failed("budget-turn", failure).map_err(|error| error.to_string())?;
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
                | ChatDriverEvent::WeixinControlDelivery(_)
                | ChatDriverEvent::InputLifecycle(_)
                | ChatDriverEvent::ApprovalRequest { .. }
                | ChatDriverEvent::InputRequest { .. }
                | ChatDriverEvent::SelectionRequest { .. }
                | ChatDriverEvent::HitlRequestReleased { .. }
                | ChatDriverEvent::ExtensionReceipt(_)
                | ChatDriverEvent::CommandCellStarted { .. }
                | ChatDriverEvent::CommandCellSettled { .. }
                | ChatDriverEvent::CommandCellWatchReady { .. }
                | ChatDriverEvent::CommandCellWatchDeliveryStarted { .. }
                | ChatDriverEvent::CommandCellWatchAcknowledged { .. }
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
        fn has_valid_contract(
            &self,
            conversation_id: &str,
            turn_id: &str,
            expected_run_id: &str,
        ) -> bool {
            let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
            let terminal_count = events
                .iter()
                .filter(|event| event.payload.is_execution_terminal())
                .count();
            let source_execution = events.first().and_then(|event| match &event.payload {
                AgentEvent::ExecutionStatus(status)
                    if status.status == echo_agent::agent::ExecutionStatusKind::Started =>
                {
                    Some(status.execution_id.as_str())
                }
                _ => None,
            });
            let settled = events.last().is_some_and(|event| {
                matches!(&event.payload,
                AgentEvent::ExecutionStatus(status)
                    if status.status == echo_agent::agent::ExecutionStatusKind::Settled
                        && Some(status.execution_id.as_str()) == source_execution)
            });
            let confirmations = events
                .iter()
                .filter(|event| matches!(event.payload, AgentEvent::FinalAnswer(_)))
                .count();
            events.iter().enumerate().all(|(index, event)| {
                event.schema_version == echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION
                    && event.sequence == (index as u64).saturating_add(1)
                    && event.conversation_id.as_ref().map(|id| id.as_str()) == Some(conversation_id)
                    && event.turn_id.as_str() == turn_id
                    && event
                        .run_id
                        .as_ref()
                        .is_some_and(|id| id.as_str() == expected_run_id)
                    && !event.event_id.as_str().is_empty()
            }) && terminal_count == 1
                && confirmations == 1
                && source_execution.is_some()
                && settled
        }
    }

    #[tokio::test]
    async fn interrupted_task_progress_keeps_approved_internal_reports_without_final_synthesis()
    -> anyhow::Result<()> {
        use echo_agent::agent::{AssistantMessageBlock, AssistantPhase};
        let sink = std::sync::Arc::new(MockChatSink::default());
        let adapter = EkoTurnEventSink::new(
            sink.clone(),
            WebhookTurnObserver::new(None, "progress-source-fixture".to_string()),
            None,
            None,
            "progress-turn".to_string(),
            TurnVisibility::Internal,
        );
        let identity = EventIdentity::for_chat(
            Some("progress-conversation".to_string()),
            "progress-turn",
            "progress-root",
            None,
        )?;
        for (index, phase) in [
            AssistantPhase::Commentary,
            AssistantPhase::PartialAnswer,
            AssistantPhase::Unknown,
            AssistantPhase::Reasoning,
            AssistantPhase::FinalAnswer,
        ]
        .into_iter()
        .enumerate()
        {
            let sequence = u64::try_from(index)?.saturating_add(1);
            let event = AgentEvent::AssistantMessage {
                block: AssistantMessageBlock {
                    provider_source: None,
                    block_id: format!("progress-block-{index}"),
                    request_id: "progress-request".into(),
                    sequence,
                    phase,
                    delta: format!("approved fixture report {index}"),
                    complete: true,
                    provider: Some("progress-fixture".into()),
                    reasoning: None,
                },
            };
            adapter
                .on_event(EventEnvelope::new(&identity, sequence, None, event)?)
                .await?;
        }
        adapter
            .on_event(EventEnvelope::new(
                &identity,
                6,
                None,
                AgentEvent::FinalAnswer("finite-turn final must remain internal".into()),
            )?)
            .await?;
        adapter
            .on_event(EventEnvelope::new(
                &identity,
                7,
                None,
                AgentEvent::Error {
                    source: "fixture_interrupted".into(),
                    message: "original execution interrupted".into(),
                    failure: echo_agent::error::AgentFailure::message(
                        "fixture_interrupted",
                        "original execution interrupted",
                    ),
                },
            )?)
            .await?;
        adapter
            .stop_projector_for_test()
            .await
            .map_err(anyhow::Error::msg)?;
        let events = sink
            .events
            .lock()
            .map_err(|_| anyhow::anyhow!("fixture sink poisoned"))?;
        let phases = events
            .iter()
            .filter_map(|event| match &event.payload {
                AgentEvent::AssistantMessage { block } => Some(block.phase),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            phases,
            [
                AssistantPhase::Commentary,
                AssistantPhase::PartialAnswer,
                AssistantPhase::Unknown,
                AssistantPhase::Reasoning
            ]
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(&event.payload, AgentEvent::Error { .. }))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(&event.payload, AgentEvent::FinalAnswer(_)))
        );
        Ok(())
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
                observation: None,
            },
            AgentEvent::LlmUsage {
                model: "test-model".to_string(),
                prompt_tokens: 11,
                completion_tokens: 7,
                total_tokens: 18,
                cached_prompt_tokens: 0,
                cache_creation_prompt_tokens: 0,
                usage_reported: true,
                observation: None,
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
        let outcome = adapter
            .finish(receipt, TurnOutcome::Completed)
            .map_err(|error| error.to_string())?;
        assert_eq!(outcome.outcome, TurnOutcome::Completed);
        assert_eq!(outcome.prompt_tokens, 11);
        assert_eq!(outcome.completion_tokens, 7);
        assert_eq!(outcome.compaction_count, 1);
        assert_eq!(outcome.final_answer.as_deref(), Some("finished"));
        assert_eq!(
            outcome.final_message_id.as_ref().map(|id| id.as_str()),
            Some("receipt-turn")
        );
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
            native_progress_fixture("undelivered", 1),
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
        let outcome = adapter
            .finish(receipt, TurnOutcome::Cancelled)
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            outcome.outcome,
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
                native_progress_fixture(format!("token-{sequence}"), sequence),
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
            native_progress_fixture("undelivered", 1),
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
        let outcome = adapter
            .finish(receipt, TurnOutcome::Cancelled)
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            outcome.outcome,
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
    fn pinned_memory_generation_precedes_live_integration_lookup() -> Result<(), String> {
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
        assert!(error.to_string().contains("admission"));
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
        let _resource_scope = store.test_resource_scope();
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
                        domain_plugin: None,

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
        let _resource_scope = store.test_resource_scope();
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
                        domain_plugin: None,

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
        let _resource_scope = store.test_resource_scope();
        store
            .create_run_with_profile(
                "claim-race-run",
                "test",
                "claim-race-conversation",
                "claim-race-root",
                crate::tasks::task_runtime::DomainProfile::General,
                "preserve the winner",
                "agent_task_plan",
                crate::tasks::task_runtime::AttendedMode::Attended,
                crate::tasks::task_runtime::TaskRunExecutionProfile::conversation_turn(),
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
                    domain_plugin: None,

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
                domain_plugin: None,

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
            .map_err(|error| format!("winner task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        // ADR 0037: a completed turn on a run that never committed a plan
        // settles as Completed at turn end (previously it spun continuation
        // turns until RepeatedBlocker paused the run).
        wait_for_run_status(
            &store,
            "claim-race-run",
            crate::tasks::task_runtime::TaskRunStatus::Completed,
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn continuation_drive_returns_deferred_when_task_activity_wins_claim()
    -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;

        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let _resource_scope = store.test_resource_scope();
        let run_id = "continuation-claim-activity";
        store
            .create_run(
                run_id,
                "test",
                "continuation-claim-conversation",
                "continuation-claim-root",
                crate::tasks::task_runtime::DomainProfile::General,
                "continue after task activity",
                "agent_task_plan",
                crate::tasks::task_runtime::AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&crate::tasks::task_runtime::TaskPlan {
                domain_binding: None,

                plan_id: "continuation-claim-plan".to_string(),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: crate::tasks::task_runtime::DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256(
                    "continue after task activity",
                ),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: crate::tasks::task_runtime::ExecutionMode::Sequential,
                tasks: vec![crate::tasks::task_runtime::PlanTask {
                    id: "active-task".to_string(),
                    title: "Active task".to_string(),
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, crate::tasks::task_runtime::TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation(run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                run_id,
                "active-task",
                echo_agent::tasks::TaskStatus::Running,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("continuation-claim-conversation".to_string()),
            root_message_id: "continuation-claim-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .llm_client(Arc::new(echo_agent::testing::MockLlmClient::new()))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let outcome = drive_chat_continuation_turn(
            &agent,
            &make_turn("continue"),
            resources,
            RunTurnBinding {
                domain_plugin: None,

                run_id: Some(run_id.to_string()),
                turn_id: "continuation-claim-turn".to_string(),
                root_message_id: "continuation-claim-root".to_string(),
                origin: RunTurnOrigin::Continuation,
                transcript_visibility: TurnVisibility::Internal,
                expected_resume: None,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, RunTurnDriveOutcome::Deferred));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if store.active_run_driver_count().unwrap_or(usize::MAX) == 0
                    && store
                        .active_run_driver_receipt_count()
                        .unwrap_or(usize::MAX)
                        == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "deferred continuation did not release driver admission".to_string())?;
        let snapshot = store
            .get_run_state(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "deferred continuation run disappeared".to_string())?;
        assert!(snapshot.continuation.is_some_and(
            |continuation| continuation.deferred && continuation.active_turn.is_none()
        ));
        Ok(())
    }

    struct TaskExecuteReceiptBarrierSink {
        reached: std::sync::mpsc::SyncSender<Box<EventEnvelope>>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        parked: std::sync::atomic::AtomicBool,
    }

    // This remains a deadlock guard; synchronization is driven by the real
    // task_execute ToolResult. Cold all-feature suites can take more than five
    // seconds to reach that event under host load.
    const TASK_EXECUTE_RECEIPT_TEST_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(30);

    impl ChatSink for TaskExecuteReceiptBarrierSink {
        fn on_event(&self, event: ChatDriverEvent) -> bool {
            let ChatDriverEvent::Agent(envelope) = event else {
                return true;
            };
            if !matches!(&envelope.payload, AgentEvent::ToolResult { name, .. } if name == "task_execute")
                || self.parked.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return true;
            }
            if self.reached.send(envelope).is_err() {
                return false;
            }
            self.release
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .recv_timeout(TASK_EXECUTE_RECEIPT_TEST_TIMEOUT)
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
        let _resource_scope = store.test_resource_scope();
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
                domain_binding: None,

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
        .await
        .map_err(|error| error.to_string())?;

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
                memory_generation: None,
                human_loop_provider: None,
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
        let unchanged_starts = store
            .list_events("existing-goal", 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| event.event_type == RuntimeEventKind::RunTurnStarted)
            .collect::<Vec<_>>();
        assert_eq!(unchanged_starts.len(), 3);
        let auto_derived =
            crate::tasks::task_runtime::task_tools::formal_run_id_for_turn("auto-resume-message");
        // ADR 0037: the ordinary turn now eagerly drives its own TaskRun and
        // settles it Completed without a plan; the existing goal run above is
        // untouched.
        let auto_run = store
            .get_run(&auto_derived)
            .map_err(|error| error.to_string())?
            .ok_or("ordinary auto-resume reply turn should eagerly create a TaskRun")?;
        assert_eq!(
            auto_run.status,
            crate::tasks::task_runtime::types::TaskRunStatus::Completed
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
        let _resource_scope = store.test_resource_scope();
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
                domain_binding: None,

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
        assert!(
            conflict_error
                .to_string()
                .contains("does not match its execution scope")
        );
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
        assert!(error.to_string().contains("identity changed"));
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
        use echo_agent::subagent::SubagentDefinition;
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
        let _resource_scope = store.test_resource_scope();
        crate::tasks::task_runtime::register_task_tools_on_agent(
            &agent,
            store.clone(),
            crate::tasks::task_runtime::default_subagent_catalog(),
        )
        .await;
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
            memory_generation: None,
            human_loop_provider: None,
        });
        let outcome = drive_chat(&agent, &make_turn("run the task graph"), resources)
            .await
            .map_err(|error| error.to_string())?;
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

    // Tokio's fixed macro option names test harness threads, outside the
    // application's Subagent terminology.
    // `worker_threads` is a fixed Tokio attribute, not an EKO execution role.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawned_task_execute_retains_pool_receipt_until_outer_driver_settles()
    -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use echo_agent::subagent::SubagentDefinition;
        use std::sync::Arc;

        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("task-pool-path")
                // 原pool先由同一transport分类；不让分类消费主ReAct的task_create脚本。
                .with_response(r#"{"intent":"FALLBACK","skill":null,"confidence":1.0}"#)
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
                .with_response("pooled task graph finished")
                .with_response(r#"{"score":9.0,"passed":true,"feedback":"Exact pooled evidence inspected","suggestions":[]}"#),
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
        let _resource_scope = store.test_resource_scope();
        crate::tasks::task_runtime::register_task_tools_on_agent(
            &primary_agent,
            store.clone(),
            crate::tasks::task_runtime::default_subagent_catalog(),
        )
        .await;
        let pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(primary_agent.clone(), None, None, 3, false)
                .await,
        );
        // 复用原测试transport接缝，同步替换EKO-owned Critic；保留真实verifier与custom Critic边界。
        pool.set_llm_client_override_for_test(llm.clone()).await;
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
                .recv_timeout(TASK_EXECUTE_RECEIPT_TEST_TIMEOUT)
                .map_err(|error| format!("task_execute result barrier was not reached: {error}"))
        })
        .await
        .map_err(|error| error.to_string())?;
        if let Err(error) = &barrier_result {
            let early_outcome = tokio::time::timeout(TASK_EXECUTE_RECEIPT_TEST_TIMEOUT, drive)
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
        let original_event = barrier_result?;
        let AgentEvent::ToolResult { result, .. } = &original_event.payload else {
            return Err("pool receipt barrier did not preserve the original ToolResult".into());
        };
        assert!(
            result.success,
            "task_execute must succeed before observing its receipt: {result:?}"
        );
        let requests = llm.all_calls();
        let classifier_request = requests
            .first()
            .ok_or("missing original classification request")?;
        assert_eq!(classifier_request.len(), 1);
        assert!(
            classifier_request
                .first()
                .and_then(|message| message.content.as_text())
                .is_some_and(|text| text.starts_with("You are an intent classifier"))
        );
        let tool_counts = llm.all_tool_counts();
        assert_eq!(tool_counts.first().copied(), Some(0));
        assert!(tool_counts.get(1).is_some_and(|count| *count > 0));
        assert!(
            store
                .get_plan(
                    &crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(
                        "pool-task-turn"
                    )
                )
                .map_err(|error| error.to_string())?
                .is_some(),
            "task_create must persist the original plan"
        );
        assert_eq!(
            store.active_run_driver_receipt_count()?,
            1,
            "the framework-spawned task_execute pool receipt must belong to the outer driver; original ToolResult: {result:?}"
        );
        release_tx
            .send(())
            .map_err(|_| "task_execute result barrier receiver closed".to_string())?;
        let outcome = tokio::time::timeout(TASK_EXECUTE_RECEIPT_TEST_TIMEOUT, drive)
            .await
            .map_err(|_| "pooled chat driver did not settle".to_string())?
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(
            llm.call_count(),
            5,
            "original classifier, three ReAct requests and Critic must all run"
        );
        assert_eq!(llm.remaining(), 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        drop(foreground_execution);
        tokio::time::timeout(TASK_EXECUTE_RECEIPT_TEST_TIMEOUT, pool.shutdown())
            .await
            .map_err(|_| "pool shutdown timed out after outer settlement".to_string())??;
        Ok(())
    }

    #[tokio::test]
    async fn drive_chat_required_context_failure_pauses_existing_run_and_can_resume()
    -> Result<(), String> {
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, TaskRunExecutionProfile, TaskRunResumeIdentity,
            TaskRunStatus, TaskRuntimeStore,
        };
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(temporary.path().join(".eko/tasks"))
                .map_err(|error| error.to_string())?,
        );
        let _resources = store.test_resource_scope();
        let turn_id = "ordinary-required-context";
        let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(turn_id);
        store
            .create_run_with_profile(
                &run_id,
                &store.active_workspace_id(),
                "c1",
                turn_id,
                DomainProfile::General,
                "保持普通会话原Goal",
                "agent_task_plan",
                AttendedMode::Attended,
                TaskRunExecutionProfile::conversation_turn(),
            )
            .map_err(|error| error.to_string())?;
        let original = format!("{}普通会话尾部限制🚀", "普通约束背景 ".repeat(2_000));
        store
            .record_run_steer(&run_id, "original-steer", &original)
            .map_err(|error| error.to_string())?;
        let artifacts = temporary.path().join(".eko/artifacts");
        std::fs::write(&artifacts, "阻断原产品数据artifact目录")
            .map_err(|error| error.to_string())?;
        let model =
            Arc::new(echo_agent::testing::MockLlmClient::new().with_response("普通回合恢复完成"));
        let agent = echo_agent::agent::ReactAgentBuilder::new()
            .model("mock-model")
            .enable_tools()
            .auto_project_rules(false)
            .llm_client(model.clone())
            .token_limit(8_000)
            .tool_output_artifacts(crate::infra::tool_output_artifact_config(Some(
                temporary.path(),
            )))
            .build()
            .map_err(|error| error.to_string())?;
        agent.set_pre_model_context_projector(Some(Arc::new(
            crate::turn_context::EkoContextProjector::new(
                crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
                crate::turn_context::turn_prompt_context_registry(),
            ),
        )));
        let agent = AgentHandle::new(agent);
        let sink = Arc::new(MockChatSink::default());
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink: sink.clone(),
            webhook_emitter: None,
            conv_id: Some("c1".to_owned()),
            root_message_id: turn_id.to_owned(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        // 走原真实drive_chat/ensure_bound_task_run/RunTurn owner，而非只调用finalize helper。
        let outcome = drive_chat(&agent, &make_turn("继续原普通任务"), resources)
            .await
            .map_err(|error| error.to_string())?;
        let TurnOutcome::Failed(failure) = outcome else {
            return Err("必要上下文错误没有沿原物理Turn失败返回".to_owned());
        };
        assert!(
            crate::tasks::task_runtime::compact_context::is_required_context_unavailable(&failure)
        );
        assert!(!failure.retryable, "不能靠自动重试标志伪造可恢复暂停");
        assert_eq!(model.call_count(), 0);
        store.wait_for_run_driver_idle(&run_id).await;
        let paused = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or("原TaskRun缺失")?;
        assert_eq!(paused.run.status, TaskRunStatus::Paused);
        assert_eq!(paused.run.goal, "保持普通会话原Goal");
        assert_eq!(
            paused
                .continuation
                .as_ref()
                .and_then(|state| state.pause.as_ref())
                .map(|pause| pause.reason),
            Some(crate::tasks::task_runtime::RunPauseReason::NeedsInput)
        );
        assert!(
            store
                .get_plan(&run_id)
                .map_err(|error| error.to_string())?
                .is_none(),
            "暂停不能强制普通聊天建图"
        );
        let ended = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|event| {
                event.event_type == crate::tasks::task_runtime::RuntimeEventKind::RunTurnFinished
            })
            .ok_or("原RunTurn终态缺失")?;
        assert_eq!(
            ended.payload.get("agent_failure"),
            Some(&serde_json::to_value(&failure).map_err(|error| error.to_string())?)
        );
        assert_eq!(
            paused
                .recent_constraints
                .first()
                .map(|steer| steer.text.as_str()),
            Some(original.as_str())
        );
        std::fs::remove_file(&artifacts).map_err(|error| error.to_string())?;
        let expected = TaskRunResumeIdentity::capture(&paused);
        let resume_turn = "ordinary-required-context-resume";
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: test_execution_scope(),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store.clone()),
            sink,
            webhook_emitter: None,
            conv_id: Some("c1".to_owned()),
            root_message_id: resume_turn.to_owned(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let resumed = drive_chat_turn(
            &agent,
            &make_turn("恢复原普通任务"),
            resources,
            Some(RunTurnBinding::resume_expected(expected, resume_turn)),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert_eq!(resumed.outcome, TurnOutcome::Completed);
        store.wait_for_run_driver_idle(&run_id).await;
        let after = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or("恢复后Run缺失")?;
        assert_eq!(after.run_id, run_id);
        assert_eq!(after.goal, "保持普通会话原Goal");
        assert_eq!(model.call_count(), 1);
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
        let _resource_scope = store.test_resource_scope();
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
            memory_generation: None,
            human_loop_provider: None,
        });
        let outcome = drive_chat(&agent, &make_turn("hi"), res)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(outcome, TurnOutcome::Completed);
        // The agent's FinalAnswer is streamed through the sink.
        assert!(
            chat_sink.has_final_answer(),
            "drive_chat should stream FinalAnswer to sink; events recorded: {}",
            chat_sink.event_count()
        );
        assert!(
            chat_sink.has_valid_contract(
                "c1",
                turn_id,
                &crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(turn_id)
            ),
            "drive_chat should preserve version, identity, sequence, and exactly one terminal"
        );
        // ADR 0037: an ordinary chat turn eagerly drives its own TaskRun and,
        // with no plan revision ever committed, settles it as Completed at
        // turn end instead of entering the continuation loop.
        let formal_run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(turn_id);
        let settled_run = store
            .get_run(&formal_run_id)
            .map_err(|error| error.to_string())?
            .ok_or("ordinary chat should eagerly create a TaskRun")?;
        assert_eq!(
            settled_run.status,
            crate::tasks::task_runtime::types::TaskRunStatus::Completed
        );
        assert!(
            store
                .get_plan(&formal_run_id)
                .map_err(|error| error.to_string())?
                .is_none(),
            "ordinary chat must not synthesize a plan"
        );
        let execution_profile = store
            .get_run_state(&formal_run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "ordinary chat run state disappeared".to_string())?
            .execution_profile;
        assert!(execution_profile.is_conversation_turn());
        let shutdown_result = store.shutdown_run_drivers().await;
        assert!(
            shutdown_result.is_ok(),
            "eager chat run driver should settle normally: {shutdown_result:?}"
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
        match &outcome.outcome {
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
        assert!(!webhook.should_emit_chat_completed(&receipt, &outcome.outcome));
        Ok(())
    }

    #[tokio::test]
    async fn successful_input_observer_preserves_completed_driver_outcome() -> Result<(), String> {
        let (outcome, sink) =
            drive_successful_model_with_observer("observer-success-turn", Ok(())).await?;

        assert!(sink.has_final_answer());
        assert_eq!(outcome.outcome, TurnOutcome::Completed);
        assert_eq!(outcome.final_answer.as_deref(), Some("model completed"));
        let webhook = WebhookTurnObserver::new(None, "observer-terminal".to_string());
        let mut receipt = test_turn_receipt("observer-success-turn", TurnOutcome::Completed)?;
        receipt.final_answer = Some("model completed".to_string());
        assert!(webhook.should_emit_chat_completed(&receipt, &outcome.outcome));
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
                    StreamChunk::Delta(Box::new(DeltaMessage {
                        role: Some("assistant".to_string()),
                        content: Some("partial answer".to_string()),
                        ..DeltaMessage::default()
                    })),
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
            memory_generation: None,
            human_loop_provider: None,
        });

        let outcome = drive_chat(&agent, &make_turn("start"), resources)
            .await
            .map_err(|error| error.to_string())?;
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
                AgentEvent::AssistantMessage { block } => Some(block.delta.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(partial_output, "partial answer");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.payload, AgentEvent::Error { .. }))
                .count(),
            1
        );
        let source_execution = events.first().and_then(|event| match &event.payload {
            AgentEvent::ExecutionStatus(status)
                if status.status == echo_agent::agent::ExecutionStatusKind::Started =>
            {
                Some(status.execution_id.as_str())
            }
            _ => None,
        });
        assert!(source_execution.is_some());
        assert!(matches!(events.last().map(|event| &event.payload),
            Some(AgentEvent::ExecutionStatus(status))
                if status.status == echo_agent::agent::ExecutionStatusKind::Failed
                    && Some(status.execution_id.as_str()) == source_execution));
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
        let _resource_scope = store.test_resource_scope();
        store
            .create_run_with_profile(
                &run_id,
                "test",
                "c1",
                turn_id,
                DomainProfile::General,
                "boundary goal",
                "agent_task_plan",
                AttendedMode::Attended,
                crate::tasks::task_runtime::TaskRunExecutionProfile::conversation_turn(),
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                domain_binding: None,

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
            memory_generation: None,
            human_loop_provider: None,
        });

        // ADR 0037: the ordinary turn eagerly claims this pre-seeded run (its
        // identity matches the turn-derived formal run) and drives it to a
        // successful completion; the old "abandoned Pending run fails" path no
        // longer exists because no turn executes outside the runtime.
        let outcome = drive_chat(&agent, &make_turn("continue"), res)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(outcome, echo_agent::runtime::TurnOutcome::Completed);
        let settled_run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "projection fixture run disappeared during settlement".to_string())?;
        if matches!(
            settled_run.status,
            crate::tasks::task_runtime::TaskRunStatus::Failed
                | crate::tasks::task_runtime::TaskRunStatus::Cancelled
        ) {
            return Err(format!(
                "projection fixture run was not driven to a healthy state: {:?}",
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
        let _resource_scope = store.test_resource_scope();
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
            memory_generation: None,
            human_loop_provider: None,
        });
        let turn = make_turn("hi there");
        drive_chat(&agent, &turn, res)
            .await
            .map_err(|error| error.to_string())?;
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
            memory_generation: None,
            human_loop_provider: None,
        });

        drive_chat(&agent, &make_turn("probe"), resources)
            .await
            .map_err(|error| error.to_string())?;
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
    async fn runless_project_turns_restore_current_target_rules_and_keep_product_data_local()
    -> anyhow::Result<()> {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;
        let project =
            crate::tasks::task_runtime::project_environment_tests::EnvironmentFixture::new()
                .map_err(anyhow::Error::msg)?;
        // Keep VCS discovery inside the modeled target namespace while the
        // same-named local directory remains the deliberately wrong target.
        crate::tasks::task_runtime::worktree::run_git(
            &project.target_root,
            &["init", "-b", "main"],
        )?;
        let client = Arc::new(
            echo_agent::testing::MockLlmClient::new().with_responses(["first done", "second done"]),
        );
        let states: Arc<dyn echo_agent::state::RuntimeStateStore> = Arc::new(
            echo_agent::state::FileRuntimeStateStore::new(project.data_root.join("checkpoints"))?,
        );
        let mut react = echo_agent::agent::ReactAgentBuilder::new()
            .model("project-turn-test")
            .system_prompt("PRIMARY_ROLE_BASELINE")
            .auto_project_rules(false)
            .file_system(project.files.clone())
            .working_dir(project.visible_root.clone())
            .llm_client(client.clone())
            .state_store(states.clone())
            .conversation_id("runless-project-checkpoint")
            .react_checkpoint_interval(1)
            .max_iterations(1)
            .build()?;
        react.set_sandbox_executor(project.executor.clone());
        react.set_pre_model_context_projector(Some(Arc::new(
            crate::turn_context::EkoContextProjector::new(
                crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
                crate::turn_context::turn_prompt_context_registry(),
            ),
        )));
        let agent = AgentHandle::new(react);
        for (name, rule) in [
            ("first", "FIRST_TARGET_RULE"),
            ("second", "SECOND_TARGET_RULE"),
            ("unavailable", "MUST_NOT_REACH_MODEL"),
        ] {
            let cwd = project.visible_root.join(name);
            project.files.create_dir_all(&cwd)?;
            project
                .files
                .write(&cwd.join("Cargo.toml"), b"fixture marker")?;
            project
                .files
                .write(&cwd.join("AGENTS.md"), rule.as_bytes())?;
            std::fs::create_dir_all(&cwd)?;
            std::fs::write(cwd.join("AGENTS.md"), "WRONG_LOCAL_RULE")?;
            let turn_id = format!("runless-project-{name}");
            let sink = Arc::new(MockChatSink::default());
            let resources = Arc::new(crate::chat_resources::ChatResources {
                execution_scope: crate::workspace::WorkspaceExecutionScope::global(cwd),
                workspace_io_receipt: Some(
                    crate::state::ScopedWorkspaceIoReceipt::global_for_test(
                        project.data_root.clone(),
                    ),
                ),
                pool: None,
                store: None,
                sink: sink.clone(),
                webhook_emitter: None,
                conv_id: Some("runless-project-checkpoint".to_string()),
                root_message_id: turn_id.clone(),
                attachments: Vec::new(),
                cancel: CancellationToken::new(),
                review_integration: None,
                memory_generation: None,
                human_loop_provider: None,
            });
            if name == "unavailable" {
                project
                    .fail_metadata
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            let result = drive_chat(&agent, &make_turn("continue"), resources).await;
            if name == "unavailable" {
                assert!(result.is_err());
                assert_eq!(client.all_calls().len(), 2);
                assert!(!crate::turn_context::turn_prompt_context_registry().contains(&turn_id));
                continue;
            }
            let outcome = result.map_err(|error| anyhow::anyhow!(error.to_string()))?;
            assert_eq!(outcome, TurnOutcome::Completed);
            assert!(
                states
                    .get_checkpoint("runless-project-checkpoint")
                    .await?
                    .is_some()
            );
            let calls = client.all_calls();
            let text = calls
                .last()
                .ok_or_else(|| anyhow::anyhow!("model call absent"))?
                .iter()
                .filter_map(|message| message.content.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.matches(rule).count(), 1);
            assert!(!text.contains("WRONG_LOCAL_RULE"));
            if name == "second" {
                assert!(!text.contains("FIRST_TARGET_RULE"));
            }
            assert!(text.contains(crate::turn_context::TURN_CONTRACT_MARKER));
            assert!(text.contains("PRIMARY_ROLE_BASELINE"));
            assert!(
                sink.events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .iter()
                    .all(|event| event.run_id.is_none())
            );
            assert!(!crate::turn_context::turn_prompt_context_registry().contains(&turn_id));
        }
        assert_eq!(client.all_calls().len(), 2);
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
        if !error
            .to_string()
            .contains("does not match TaskRuntime workspace")
        {
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
                EventEnvelope::new(&identity, 1, None, native_progress_fixture("hel", 1),)
                    .map_err(|error| error.to_string())?
            ))),
            "on_event should return true to continue"
        );
        assert!(
            sink.on_event(ChatDriverEvent::Agent(Box::new(
                EventEnvelope::new(&identity, 2, None, native_progress_fixture("lo", 2),)
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
            AgentEvent::AssistantMessage { block } => assert_eq!(block.delta, "hel"),
            other => {
                return Err(format!(
                    "first should be native Commentary(hel); got {other:?}"
                ));
            }
        }
        match second.payload {
            AgentEvent::AssistantMessage { block } => assert_eq!(block.delta, "lo"),
            other => {
                return Err(format!(
                    "second should be native Commentary(lo); got {other:?}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn explicit_resume_selection_compares_the_complete_original_domain_binding()
    -> Result<(), String> {
        let original = crate::domain_plugin::test_binding(false)?;
        check_resume_domain_selection(None, None, Some(&original))?;
        check_resume_domain_selection(
            Some(&original.snapshot.plugin.plugin_id),
            Some(&original),
            Some(&original),
        )?;
        let mut changed = original.snapshot.clone();
        changed.method.push_str(" Changed method.");
        let changed = crate::domain_plugin::DomainBinding::new(changed)?;
        assert!(
            check_resume_domain_selection(
                Some(&original.snapshot.plugin.plugin_id),
                Some(&changed),
                Some(&original)
            )
            .is_err()
        );
        assert!(
            check_resume_domain_selection(Some("another-domain"), Some(&changed), Some(&original))
                .is_err()
        );
        assert!(
            check_resume_domain_selection(
                Some(&original.snapshot.plugin.plugin_id),
                Some(&original),
                None
            )
            .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn resume_domain_conflict_rejects_before_claim_and_attachment_mutation()
    -> Result<(), String> {
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, RunPlanPolicy, TaskRunExecutionProfile,
        };
        use std::sync::Arc;
        for original_domain in [false, true] {
            let store = Arc::new(
                crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                    .map_err(|error| error.to_string())?,
            );
            let _resource_scope = store.test_resource_scope();
            let original = original_domain
                .then(|| crate::domain_plugin::test_binding(false))
                .transpose()?;
            store
                .create_run_for_active_workspace_with_domain(
                    "domain-resume",
                    "conversation",
                    "root",
                    DomainProfile::General,
                    "original goal",
                    "agent_task_plan",
                    AttendedMode::Attended,
                    TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
                    original.as_ref(),
                )
                .map_err(|error| error.to_string())?;
            store
                .transition_run(
                    "domain-resume",
                    crate::tasks::task_runtime::TaskRunStatus::Running,
                )
                .map_err(|error| error.to_string())?;
            store
                .configure_run_continuation("domain-resume", true, false, None, None)
                .map_err(|error| error.to_string())?;
            store
                .request_pause("domain-resume")
                .map_err(|error| error.to_string())?;
            let before = store
                .get_run_state("domain-resume")
                .map_err(|error| error.to_string())?
                .ok_or("resume run missing")?;
            let before_events = serde_json::to_value(
                store
                    .list_events("domain-resume", 0)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let expected = crate::tasks::task_runtime::TaskRunResumeIdentity::capture(&before);
            let resources = Arc::new(crate::chat_resources::ChatResources {
                execution_scope: test_execution_scope(),
                workspace_io_receipt: None,
                pool: None,
                store: Some(store.clone()),
                sink: Arc::new(MockChatSink::default()),
                webhook_emitter: None,
                conv_id: Some("conversation".to_string()),
                root_message_id: "resume-turn".to_string(),
                attachments: vec![crate::attachments::AttachmentRef {
                    path: std::path::PathBuf::from("/tmp/domain-resume-not-admitted"),
                    name: "not-admitted.txt".to_string(),
                    mime_type: "text/plain".to_string(),
                    source: Default::default(),
                }],
                cancel: echo_agent::agent::CancellationToken::new(),
                review_integration: None,
                memory_generation: None,
                human_loop_provider: None,
            });
            let mut selection = RunTurnBinding::resume_expected(expected, "resume-turn");
            selection.domain_plugin = Some("another-domain".to_string());
            let rejected =
                prepare_chat_execution(&make_turn("resume"), resources, Some(selection)).await;
            let error = match rejected {
                Err(error) => error.to_string(),
                Ok(_) => return Err("conflicting domain resume unexpectedly prepared".to_string()),
            };
            assert!(
                error.contains(if original_domain {
                    "conflicts with the original TaskRun binding"
                } else {
                    "existing General TaskRun cannot acquire a domain"
                }),
                "resume must reject the explicit binding conflict before pool lookup"
            );
            let after = store
                .get_run_state("domain-resume")
                .map_err(|error| error.to_string())?
                .ok_or("resume run disappeared")?;
            assert_eq!(after.run.status, before.run.status);
            assert_eq!(after.run.domain_binding, before.run.domain_binding);
            assert!(after.run.attachments.is_empty());
            assert_eq!(
                serde_json::to_value(
                    store
                        .list_events("domain-resume", 0)
                        .map_err(|error| error.to_string())?
                )
                .map_err(|error| error.to_string())?,
                before_events
            );
            assert_eq!(store.active_run_driver_count()?, 0);
        }
        Ok(())
    }
    #[tokio::test]
    async fn startup_actual_factory_and_driver_read_rules_once_per_turn_and_refresh_external_edits()
    -> anyhow::Result<()> {
        use crate::project::startup_tests::CountingFiles;
        use std::sync::Arc;
        use std::sync::atomic::Ordering;
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("AGENTS.md"), "STARTUP_FIRST_RULE")?;
        let files = Arc::new(CountingFiles::new(root.clone()));
        let store = crate::project::startup_tests::task_store(files.clone())?;
        let _resource_scope = store.test_resource_scope();
        let mut created = crate::infra::create_agent_with_diagnostics(
            &crate::infra::AgentCreateParams {
                context_prepared_by_invocation: true,
                file_system: Some(files.clone()),
                sandbox_executor: store
                    .project_environment()
                    .map(|environment| environment.executor),
                task_runtime_store: Some(store.clone()),
                project: Some(root.to_string_lossy().into_owned()),
                working_dir: Some(root.clone()),
                workspace_data_root: Some(temp.path().join("data")),
                conversation_id: Some("startup-current-rules".into()),
                ..Default::default()
            },
            &crate::config::EkoConfig::default(),
        )
        .await
        .map_err(anyhow::Error::msg)?;
        assert_eq!(
            files.count("read", &root.join("AGENTS.md")),
            0,
            "factory must leave invocation facts to the actual driver"
        );
        let client = Arc::new(
            echo_agent::testing::MockLlmClient::new().with_responses(["first done", "second done"]),
        );
        created.agent.set_llm_client(client.clone());
        let agent = AgentHandle::new(created.agent);
        for (index, rule) in ["STARTUP_FIRST_RULE", "STARTUP_SECOND_RULE"]
            .into_iter()
            .enumerate()
        {
            std::fs::write(root.join("AGENTS.md"), rule)?;
            let resources = Arc::new(crate::chat_resources::ChatResources {
                execution_scope: crate::workspace::WorkspaceExecutionScope::global(root.clone()),
                workspace_io_receipt: None,
                pool: None,
                store: None,
                sink: Arc::new(MockChatSink::default()),
                webhook_emitter: None,
                conv_id: Some("startup-current-rules".into()),
                root_message_id: format!("startup-{index}"),
                attachments: Vec::new(),
                cancel: echo_agent::agent::CancellationToken::new(),
                review_integration: None,
                memory_generation: None,
                human_loop_provider: None,
            });
            let outcome = drive_chat(&agent, &make_turn("你好"), resources)
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            assert_eq!(outcome, TurnOutcome::Completed);
            let text = client
                .last_messages()
                .ok_or_else(|| anyhow::anyhow!("missing model request"))?
                .iter()
                .filter_map(|m| m.content.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.matches(rule).count(), 1);
            if index == 1 {
                assert!(!text.contains("STARTUP_FIRST_RULE"));
            }
            assert_eq!(
                files.count("read", &root.join("AGENTS.md")),
                index.saturating_add(1)
            );
            // 根探测只访问已知 marker；构造不扫描结构，每条消息只扫描一次。
            assert_eq!(files.count("read_dir", &root), index.saturating_add(1));
            assert_eq!(files.opened.load(Ordering::SeqCst), index.saturating_add(2));
            assert_eq!(
                files.closed.load(Ordering::SeqCst),
                files.opened.load(Ordering::SeqCst)
            );
        }
        assert_eq!(client.call_count(), 2);
        Ok(())
    }
}
