//! Stage 2 — shared chat driver (极简入口).
//!
//! `drive_chat` is the single, thin entry for a chat turn across TUI / CLI
//! channel / GUI: it takes a [`PreparedUserTurn`] (instruction + input
//! resources, with the mode hint already folded in and long pastes spilled to
//! a user-input artifact), collapses it into one `Message` via
//! [`PreparedUserTurn::to_message`], streams the agent's ReAct reply through a
//! per-mode `ChatSink`, and stops. It does not classify Auto requests in
//! advance. Task mode creates its required formal run before execution; Auto
//! creates a run only when the agent invokes a formal plan or long-lived task
//! tool; ordinary Chat/Auto turns create none.
//!
//! Multimodal (images/files) is delivered via the turn's inline resources; the
//! old `(&str, Option<&Message>)` pair has been replaced by the single
//! `PreparedUserTurn`.

use echo_agent::agent::{
    Agent, AgentEvent, AgentHandle, EventEnvelope, EventIdentity, envelope_event_stream,
};
use echo_agent::prelude::Message;
use echo_core::tools::TraceSinkFn;
use futures::StreamExt;

use crate::tasks::task_runtime::executor::ExecEvent;
use crate::tasks::task_runtime::types::{
    RunPauseReason, RunTurnBinding, RunTurnOrigin, RunTurnStatus, RuntimeEventKind, TaskRunStatus,
    TurnVisibility,
};

/// Complete product event stream consumed by every interactive surface.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "source", content = "event", rename_all = "snake_case")]
pub enum ChatDriverEvent {
    Agent(Box<EventEnvelope>),
    Execution(ExecEvent),
    TurnStatus {
        status: String,
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

/// Runtime-owned terminal result for one interactive turn.
///
/// The framework event envelope guarantees exactly one terminal Agent event;
/// this value carries that same fact back to the entry point so callers never
/// infer success from "the stream returned Ok".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Cancelled,
    Failed(echo_agent::error::AgentFailure),
}

impl TurnOutcome {
    pub fn status(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed(_) => "failed",
        }
    }

    fn from_agent_event(event: &AgentEvent) -> Option<Self> {
        match event {
            AgentEvent::FinalAnswer(_) => Some(Self::Completed),
            AgentEvent::Cancelled => Some(Self::Cancelled),
            AgentEvent::Error { failure, .. }
                if failure.terminal_kind == echo_agent::error::AgentTerminalKind::Cancelled =>
            {
                Some(Self::Cancelled)
            }
            AgentEvent::Error { failure, .. } => Some(Self::Failed(failure.clone())),
            _ => None,
        }
    }
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

/// Per-mode event consumer for the shared chat driver.
///
/// Each mode provides one exhaustive product-event entry point:
/// - GUI (`TauriChatSink`, in the Tauri layer) emits Tauri events.
/// - TUI/CLI render the stream to the terminal.
/// - channel aggregates by sentence + forwarding.
pub trait ChatSink: Send + Sync + 'static {
    /// Return `false` to stop the current stream because the consumer closed.
    fn on_event(&self, event: ChatDriverEvent) -> bool;

    /// Return a sink safe to retain beyond the current foreground lease. Most
    /// sinks are already durable and use the default. Foreground cancellation
    /// wrappers expose their underlying sink so later finite turns do not keep
    /// a stale cancellation token or terminal-delivery latch.
    fn continuation_sink(&self) -> Option<std::sync::Arc<dyn ChatSink>> {
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
    started: std::time::Instant,
    input_tokens: usize,
    output_tokens: usize,
    completed: bool,
    compaction_count: u32,
    final_answer: Option<String>,
    final_message_id: Option<String>,
    tools: std::collections::HashMap<String, (String, String, std::time::Instant)>,
}

impl WebhookTurnObserver {
    fn new(emitter: Option<std::sync::Arc<crate::webhook::WebhookEmitter>>, model: String) -> Self {
        Self {
            emitter,
            model,
            started: std::time::Instant::now(),
            input_tokens: 0,
            output_tokens: 0,
            completed: false,
            compaction_count: 0,
            final_answer: None,
            final_message_id: None,
            tools: std::collections::HashMap::new(),
        }
    }

    fn observe(&mut self, event: &EventEnvelope) {
        let payload = &event.payload;
        match payload {
            AgentEvent::LlmUsage {
                prompt_tokens,
                completion_tokens,
                ..
            } => {
                self.input_tokens = self.input_tokens.saturating_add(*prompt_tokens);
                self.output_tokens = self.output_tokens.saturating_add(*completion_tokens);
            }
            AgentEvent::ContextCompressed { .. } => {
                self.compaction_count = self.compaction_count.saturating_add(1);
            }
            AgentEvent::FinalAnswer(answer) => {
                self.completed = true;
                self.final_answer = Some(answer.chars().take(4_000).collect());
                self.final_message_id = event.message_id.as_ref().map(ToString::to_string);
            }
            _ => {}
        }
        let Some(emitter) = self.emitter.as_ref() else {
            return;
        };
        match payload {
            AgentEvent::LlmUsage { .. } => {}
            AgentEvent::ToolCall {
                call_id,
                name,
                args,
            } => {
                let args_summary = args.to_string().chars().take(240).collect::<String>();
                self.tools.insert(
                    call_id.clone(),
                    (name.clone(), args_summary, std::time::Instant::now()),
                );
            }
            AgentEvent::ToolResult { call_id, name, .. } => {
                let (tool_name, args_summary, started) = self
                    .tools
                    .remove(call_id)
                    .unwrap_or_else(|| (name.clone(), String::new(), std::time::Instant::now()));
                emitter.emit(crate::webhook::WebhookEvent::ToolCalled {
                    name: tool_name,
                    args_summary,
                    elapsed_ms: duration_millis(started.elapsed()),
                });
            }
            AgentEvent::ToolError {
                call_id,
                name,
                error,
                ..
            } => {
                self.tools.remove(call_id);
                emitter.emit(crate::webhook::WebhookEvent::ToolFailed {
                    name: name.clone(),
                    error: error.clone(),
                });
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

    fn finish(self, turn_id: String, terminal: TurnOutcome) -> ChatTurnOutcome {
        let elapsed = self.started.elapsed();
        if self.completed
            && let Some(emitter) = self.emitter
        {
            emitter.emit(crate::webhook::WebhookEvent::ChatCompleted {
                model: self.model,
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                elapsed_ms: duration_millis(elapsed),
            });
        }
        ChatTurnOutcome {
            turn_id,
            terminal,
            input_tokens: u64::try_from(self.input_tokens).unwrap_or(u64::MAX),
            output_tokens: u64::try_from(self.output_tokens).unwrap_or(u64::MAX),
            compaction_count: self.compaction_count,
            elapsed_seconds: duration_seconds_rounded_up(elapsed),
            final_answer: self.final_answer,
            final_message_id: self.final_message_id,
        }
    }
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
/// Task mode creates its formal TaskRun immediately; Auto creates one lazily
/// only when the agent chooses a formal plan or long-lived run.
///
/// ## turn/run identity
///
/// 普通 chat 轮次使用 `res.root_message_id` 作 turn_id。Task mode 和 task tools
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
    wait_for_previous_continuation_driver(&res, binding.as_ref()).await?;
    match prepare_chat_execution(turn, res, binding)? {
        ChatExecutionPreparation::Ready(prepared) => {
            drive_prepared_chat(agent.clone(), turn, prepared, None).await
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
    let prepared = match prepare_chat_execution(turn, res, binding)? {
        ChatExecutionPreparation::Ready(prepared) => prepared,
        ChatExecutionPreparation::Settled(outcome) => return Ok(outcome),
    };
    let acquire_cancel = prepared.cancel.clone();
    let execution = match tokio::select! {
        biased;
        _ = acquire_cancel.cancelled() => {
            prepared.reject_before_driver_start("continuation cancelled during pool admission")?;
            return Err("continuation cancelled during pool admission".to_string());
        }
        result = pool.acquire(pool_key) => result,
    } {
        Ok(execution) => execution,
        Err(error) => {
            let message = format!("AgentPool admission failed: {error}");
            prepared.reject_before_driver_start(&message)?;
            return Err(message);
        }
    };
    let agent = execution.agent();
    let configure_cancel = prepared.cancel.clone();
    let configuration = tokio::select! {
        biased;
        _ = configure_cancel.cancelled() => {
            prepared.reject_before_driver_start("continuation cancelled during agent configuration")?;
            return Err("continuation cancelled during agent configuration".to_string());
        }
        result = configure(agent.clone()) => result,
    };
    if let Err(error) = configuration {
        prepared.reject_before_driver_start(&error)?;
        return Err(error);
    }
    drive_prepared_chat(agent, turn, prepared, Some(execution)).await
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
    let implicit_resume = if explicit_resume.is_none()
        && matches!(
            resources.interaction_mode,
            crate::tasks::task_runtime::InteractionMode::Task
                | crate::tasks::task_runtime::InteractionMode::Auto
        ) {
        resources
            .conv_id
            .as_deref()
            .map(|conversation_id| {
                store
                    .find_in_progress_run_by_conversation(conversation_id)
                    .map_err(|error| error.to_string())
            })
            .transpose()?
            .flatten()
            .filter(|run| run.status == TaskRunStatus::Paused)
            .and_then(|run| {
                store
                    .get_run_state(&run.run_id)
                    .ok()
                    .flatten()
                    .and_then(|state| state.continuation)
                    .is_some_and(|continuation| continuation.enabled)
                    .then_some(run.run_id)
            })
    } else {
        None
    };
    if let Some(run_id) = explicit_resume.or(implicit_resume) {
        store.wait_for_run_driver_idle(&run_id).await;
    }
    Ok(())
}

enum ChatExecutionPreparation {
    Ready(PreparedChatExecution),
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
    interaction_mode: crate::tasks::task_runtime::InteractionMode,
    drives_task_run: bool,
    store: Option<std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
}

impl PreparedChatExecution {
    /// A pool/configuration rejection happens after the RunTurn claim but
    /// before model execution. Preserve the controlling intent: application
    /// shutdown is boot-recoverable, explicit cancellation is terminal, and
    /// only a live provider failure becomes `ProviderUnavailable`.
    fn reject_before_driver_start(mut self, detail: &str) -> Result<(), String> {
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
        let admission_open = store.is_run_driver_admission_open();
        let cancelled = self.cancel.is_cancelled();
        let turn_status = if cancelled {
            RunTurnStatus::Cancelled
        } else {
            RunTurnStatus::Failed
        };
        let fingerprint = if !admission_open {
            "runtime_shutdown"
        } else if cancelled {
            "user_cancelled"
        } else {
            "continuation_admission"
        };
        let settlement = store
            .finish_run_turn(
                &self.formal_run_id,
                crate::tasks::task_runtime::store::RunTurnCompletion {
                    turn_id: &self.turn_id,
                    status: turn_status,
                    elapsed_seconds: 0,
                    final_message_id: None,
                    error_fingerprint: Some(fingerprint),
                },
            )
            .map_err(|error| error.to_string())
            .and_then(|_| {
                let run_is_running = store
                    .get_run(&self.formal_run_id)
                    .map_err(|error| error.to_string())?
                    .is_some_and(|run| run.status == TaskRunStatus::Running);
                if !run_is_running {
                    return Ok(());
                }
                if !admission_open {
                    store
                        .request_pause_with_reason(
                            &self.formal_run_id,
                            RunPauseReason::BootRecovery,
                            Some("application shutdown interrupted pre-driver admission"),
                        )
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                } else if cancelled {
                    store
                        .transition_run(&self.formal_run_id, TaskRunStatus::Cancelled)
                        .map_err(|error| error.to_string())?;
                    crate::tasks::task_runtime::continuation::clear_launcher(
                        store,
                        &self.formal_run_id,
                    );
                    crate::tasks::task_runtime::command_cells::stop_cells_for_run(
                        &self.formal_run_id,
                    );
                    Ok(())
                } else {
                    store
                        .request_pause_with_reason(
                            &self.formal_run_id,
                            RunPauseReason::ProviderUnavailable,
                            Some(detail),
                        )
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                }
            });
        if let Some(registration) = self.task_driver_registration.take() {
            registration.reject(detail.to_string());
        }
        settlement
    }
}

fn prepare_chat_execution(
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    binding: Option<RunTurnBinding>,
) -> Result<ChatExecutionPreparation, String> {
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
    let mut binding = binding.unwrap_or_else(|| RunTurnBinding {
        run_id: None,
        turn_id: default_turn_id.clone(),
        root_message_id: default_turn_id,
        origin: RunTurnOrigin::User,
        transcript_visibility: TurnVisibility::Visible,
    });
    if matches!(
        res.interaction_mode,
        crate::tasks::task_runtime::InteractionMode::Task
            | crate::tasks::task_runtime::InteractionMode::Auto
    ) && binding.run_id.is_none()
        && let (Some(store), Some(conversation_id)) = (res.store.as_ref(), res.conv_id.as_deref())
        && let Some(existing) = store
            .find_in_progress_run_by_conversation(conversation_id)
            .map_err(|error| error.to_string())?
        && existing.status == crate::tasks::task_runtime::TaskRunStatus::Paused
        && store
            .get_run_state(&existing.run_id)
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .is_some_and(|continuation| continuation.enabled)
    {
        binding.run_id = Some(existing.run_id);
        binding.root_message_id = existing.root_message_id;
        binding.origin = RunTurnOrigin::Resume;
    }
    if binding.turn_id.trim().is_empty() || binding.root_message_id.trim().is_empty() {
        return Err("RunTurn binding requires non-empty turn and root message ids".to_string());
    }
    let turn_id = binding.turn_id.clone();
    let drives_task_run = res.interaction_mode == crate::tasks::task_runtime::InteractionMode::Task
        || binding.run_id.is_some();
    let formal_run_id = binding.run_id.clone().unwrap_or_else(|| {
        crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&turn_id)
    });
    // Every turn that can reach task_create/task_execute is registered before
    // memory admission. Task mode requires a run; Chat/Auto permit an ordinary
    // run-less turn but supervise any lazily-created TaskRun with this token.
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
        .map(|lease| std::sync::Arc::new(lease.create_layer_manager()))
        .or_else(|| res.layer_manager.clone());
    let res = std::sync::Arc::new(crate::chat_resources::ChatResources {
        pool: res.pool.clone(),
        store: res.store.clone(),
        sink: res.sink.clone(),
        webhook_emitter: res.webhook_emitter.clone(),
        conv_id: res.conv_id.clone(),
        root_message_id: res.root_message_id.clone(),
        attachments: res.attachments.clone(),
        cancel: res.cancel.clone(),
        interaction_mode: res.interaction_mode,
        review_integration: res.review_integration.clone(),
        layer_manager,
        memory_generation,
        human_loop_provider: res.human_loop_provider.clone(),
    });
    let cancel = res.cancel.clone();
    let sink = res.sink.clone();
    let trace_sink = subagent_trace_sink_for(&sink);
    let interaction_mode = res.interaction_mode;
    let store = res.store.clone();
    if drives_task_run {
        if let Some(registration) = task_driver_registration.as_mut() {
            registration.mark_preparation_started();
        }
        // The raw prepared instruction is the goal. For spilled long text it is
        // the reference block, which is a better task goal than the full paste.
        // Dynamic mode policy stays in the per-turn context projection.
        if let Err(error) = ensure_task_mode_run(
            store.as_ref(),
            &formal_run_id,
            res.conv_id.as_deref(),
            &binding.root_message_id,
            &turn.instruction,
            &res.attachments,
            Some(&trace_sink),
        ) {
            if let Some(registration) = task_driver_registration.take() {
                registration.fail_preparation(error.clone());
            }
            return Err(error);
        }
        let continuation = store
            .as_ref()
            .and_then(|store| store.get_run_state(&formal_run_id).ok().flatten())
            .and_then(|state| state.continuation);
        if continuation.is_none()
            && let Some(store) = store.as_ref()
        {
            store
                .configure_run_continuation(&formal_run_id, true, false, None, None)
                .map_err(|error| error.to_string())?;
        }
        if let Some(store) = store.as_ref() {
            let should_resume = matches!(
                binding.origin,
                RunTurnOrigin::Resume | RunTurnOrigin::Recovery
            ) && store
                .get_run(&formal_run_id)
                .map_err(|error| error.to_string())?
                .is_some_and(|run| run.status == crate::tasks::task_runtime::TaskRunStatus::Paused);
            if should_resume && let Err(error) = store.resume_task_run(&formal_run_id) {
                let message = error.to_string();
                if let Some(registration) = task_driver_registration.take() {
                    registration.reject(message.clone());
                }
                return Err(message);
            }
            match store
                .claim_run_turn(
                    &formal_run_id,
                    &turn_id,
                    binding.origin,
                    binding.transcript_visibility,
                )
                .map_err(|error| error.to_string())?
            {
                crate::tasks::task_runtime::store::RunTurnClaimOutcome::Started(_) => {}
                crate::tasks::task_runtime::store::RunTurnClaimOutcome::NotSubmitted(reason) => {
                    if let Some(registration) = task_driver_registration.take() {
                        registration.reject(format!("RunTurn was not submitted: {reason:?}"));
                    }
                    return Err(format!("RunTurn was not submitted: {reason:?}"));
                }
            }
        }
    }
    Ok(ChatExecutionPreparation::Ready(PreparedChatExecution {
        turn_id,
        formal_run_id,
        binding,
        task_driver_registration,
        resources: res,
        cancel,
        sink,
        trace_sink,
        interaction_mode,
        drives_task_run,
        store,
    }))
}

async fn drive_prepared_chat(
    agent: AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    mut prepared: PreparedChatExecution,
    pool_execution: Option<crate::agent_pool::AgentPoolExecutionLease>,
) -> Result<ChatTurnOutcome, String> {
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
        && let Some(store) = prepared.store.as_ref()
    {
        crate::tasks::task_runtime::continuation::register_launcher(
            store,
            &prepared.formal_run_id,
            agent.clone(),
            prepared.resources.clone(),
            prepared.binding.root_message_id.clone(),
        );
    }
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
            ),
        )
        .await
    };
    if prepared.drives_task_run
        && let Some(store) = prepared.store.as_ref()
    {
        let outcome = crate::tasks::task_runtime::continuation::request_continue(
            store,
            &prepared.formal_run_id,
            RunTurnOrigin::Continuation,
        );
        tracing::debug!(
            run_id = %prepared.formal_run_id,
            ?outcome,
            "finite RunTurn requested continuation"
        );
    }
    let requested_mode = prepared.interaction_mode.as_str();
    let observed_path = observe_execution_path(
        prepared.store.as_ref(),
        &prepared.formal_run_id,
        &prepared.turn_id,
        requested_mode,
    );
    let _ = prepared.sink.on_event(ChatDriverEvent::ExecutionPath {
        requested_mode: requested_mode.to_string(),
        observed_path: observed_path.to_string(),
    });
    tracing::info!(
        requested_mode,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunTurnDecision {
    Stop,
    Continue,
}

async fn drive_registered_turn(driver: RegisteredTurnDriver) -> Result<ChatTurnOutcome, String> {
    crate::foreground_turn::advance_current_agent_turn(&driver.binding.turn_id);
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
    )?;
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

fn ensure_task_mode_run(
    store: Option<&std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    run_id: &str,
    conversation_id: Option<&str>,
    turn_id: &str,
    goal: &str,
    attachments: &[crate::attachments::AttachmentRef],
    trace_sink: Option<&crate::tasks::task_runtime::task_tools::TraceSink>,
) -> Result<(), String> {
    use crate::tasks::task_runtime::{AttendedMode, DomainProfile, TaskRunStatus};

    let store = store.ok_or_else(|| "Task mode requires TaskRuntimeStore".to_string())?;
    let run = store
        .create_run_for_active_workspace(
            run_id,
            conversation_id.unwrap_or("message:task"),
            turn_id,
            DomainProfile::General,
            goal,
            "agent_task_plan",
            AttendedMode::Attended,
        )
        .map_err(|error| error.to_string())?;
    if !attachments.is_empty() {
        store
            .set_run_attachments(run_id, attachments)
            .map_err(|error| error.to_string())?;
    }
    if run.status == TaskRunStatus::Pending {
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        if let Some(trace_sink) = trace_sink {
            trace_sink(ExecEvent::run(
                run_id.to_string(),
                RuntimeEventKind::RunStarted,
                serde_json::json!({
                    "conversation_id": conversation_id,
                    "goal": goal,
                    "mode": "task",
                    "route": "agent_task_plan",
                }),
            ));
        }
    }
    Ok(())
}

fn finalize_run_turn(
    store: &std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
    run_id: &str,
    outcome: &ChatTurnOutcome,
    trace_sink: Option<&crate::tasks::task_runtime::task_tools::TraceSink>,
) -> Result<RunTurnDecision, String> {
    use crate::tasks::task_runtime::{RunPauseReason, TaskRunStatus};

    let (status, error_fingerprint) = match &outcome.terminal {
        TurnOutcome::Completed => (RunTurnStatus::Ended, None),
        TurnOutcome::Cancelled => (RunTurnStatus::Cancelled, Some("cancelled")),
        TurnOutcome::Failed(failure) => (RunTurnStatus::Failed, Some(failure.code.as_str())),
    };
    let continuation = store
        .finish_run_turn(
            run_id,
            crate::tasks::task_runtime::store::RunTurnCompletion {
                turn_id: &outcome.turn_id,
                status,
                elapsed_seconds: outcome.elapsed_seconds,
                final_message_id: outcome.final_message_id.as_deref(),
                error_fingerprint,
            },
        )
        .map_err(|error| error.to_string())?;
    let run = store
        .get_run(run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("TaskRun disappeared while finishing RunTurn: {run_id}"))?;
    if run.status != TaskRunStatus::Running {
        return Ok(RunTurnDecision::Stop);
    }
    if continuation.enabled && !store.is_run_driver_admission_open() {
        let _paused = store
            .request_pause_with_reason(
                run_id,
                RunPauseReason::BootRecovery,
                Some("application shutdown interrupted an active continuation turn"),
            )
            .map_err(|error| error.to_string())?;
        return Ok(RunTurnDecision::Stop);
    }

    match &outcome.terminal {
        TurnOutcome::Cancelled => {
            store
                .transition_run(run_id, TaskRunStatus::Cancelled)
                .map_err(|error| error.to_string())?;
            crate::tasks::task_runtime::command_cells::stop_cells_for_run(run_id);
            if let Some(trace_sink) = trace_sink {
                trace_sink(ExecEvent::run(
                    run_id.to_string(),
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({ "status": "cancelled", "mode": "task" }),
                ));
            }
            return Ok(RunTurnDecision::Stop);
        }
        TurnOutcome::Failed(failure) => {
            if failure.retryable {
                let reason = if failure.category == echo_agent::error::AgentFailureCategory::Llm {
                    RunPauseReason::ProviderUnavailable
                } else {
                    RunPauseReason::NeedsInput
                };
                let _paused = store
                    .request_pause_with_reason(run_id, reason, Some(&failure.message))
                    .map_err(|error| error.to_string())?;
            } else {
                store
                    .transition_run(run_id, TaskRunStatus::Failed)
                    .map_err(|error| error.to_string())?;
            }
            return Ok(RunTurnDecision::Stop);
        }
        TurnOutcome::Completed => {}
    }

    if continuation
        .token_budget
        .is_some_and(|budget| continuation.tokens_used >= budget)
    {
        let _paused = store
            .request_pause_with_reason(
                run_id,
                RunPauseReason::TokenBudget,
                Some("the configured long-horizon token budget was exhausted"),
            )
            .map_err(|error| error.to_string())?;
        return Ok(RunTurnDecision::Stop);
    }
    if continuation
        .time_budget_seconds
        .is_some_and(|budget| continuation.time_used_seconds >= budget)
    {
        let _paused = store
            .request_pause_with_reason(
                run_id,
                RunPauseReason::TimeBudget,
                Some("the configured long-horizon time budget was exhausted"),
            )
            .map_err(|error| error.to_string())?;
        return Ok(RunTurnDecision::Stop);
    }
    let active_cells = store
        .list_background_cells(run_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(crate::tasks::task_runtime::BackgroundCellState::is_active)
        .count();
    if active_cells > 0 {
        store
            .set_continuation_deferred(run_id, true)
            .map_err(|error| error.to_string())?;
        tracing::info!(
            run_id,
            active_cells,
            "long-horizon continuation deferred until background cells settle"
        );
        return Ok(RunTurnDecision::Stop);
    }
    if continuation
        .blocker_audit
        .as_ref()
        .is_some_and(|audit| audit.consecutive_turns >= 3)
    {
        let _paused = store
            .request_pause_with_reason(
                run_id,
                RunPauseReason::RepeatedBlocker,
                Some("three consecutive RunTurns ended without TaskRuntime progress"),
            )
            .map_err(|error| error.to_string())?;
        return Ok(RunTurnDecision::Stop);
    }
    if continuation.enabled && !continuation.deferred {
        return Ok(RunTurnDecision::Continue);
    }

    let reason = match store.get_plan(run_id) {
        Ok(Some(_)) => "Task mode turn ended before task_execute reached a terminal result",
        _ => "Task mode turn ended without creating a formal plan",
    };
    store
        .note(run_id, None, reason)
        .map_err(|error| error.to_string())?;
    store
        .transition_run(run_id, TaskRunStatus::Failed)
        .map_err(|error| error.to_string())?;
    if let Some(trace_sink) = trace_sink {
        trace_sink(ExecEvent::run(
            run_id.to_string(),
            RuntimeEventKind::RunFailed,
            serde_json::json!({ "error": reason, "mode": "task" }),
        ));
    }
    Ok(RunTurnDecision::Stop)
}

fn observe_execution_path(
    store: Option<&std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    formal_run_id: &str,
    turn_id: &str,
    requested_mode: &str,
) -> &'static str {
    use crate::tasks::task_runtime::TaskRunStatus;

    let Some(store) = store else {
        return "direct";
    };
    if let Ok(Some(run)) = store.get_run(formal_run_id) {
        let observed = if run.route == "agent_autonomous" {
            "detached_background"
        } else {
            "formal_plan"
        };
        let _ = store.record_execution_path(&run.run_id, requested_mode, observed);
        return observed;
    }
    let statuses = [
        TaskRunStatus::Pending,
        TaskRunStatus::Running,
        TaskRunStatus::Paused,
        TaskRunStatus::Cancelled,
        TaskRunStatus::Failed,
        TaskRunStatus::Completed,
    ];
    let Ok(runs) = store.list_runs_in(&statuses) else {
        return "direct";
    };
    let matching: Vec<_> = runs
        .into_iter()
        .filter(|run| run.root_message_id == turn_id)
        .collect();
    let observed = if matching.iter().any(|run| run.run_id == formal_run_id) {
        "formal_plan"
    } else if matching.iter().any(|run| run.route == "agent_autonomous") {
        "detached_background"
    } else {
        "direct"
    };
    for run in matching {
        let _ = store.record_execution_path(&run.run_id, requested_mode, observed);
    }
    observed
}

struct ChatTurnModelScope {
    turn_id: String,
    bound_run_id: Option<String>,
    driver_execution_context: Option<String>,
    origin: RunTurnOrigin,
    transcript_visibility: TurnVisibility,
}

/// Inner ReAct-streaming body of [`drive_chat`], run inside the run_id scope.
async fn drive_chat_inner(
    agent: &AgentHandle,
    turn: &crate::prepared_turn::PreparedUserTurn,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    scope: ChatTurnModelScope,
) -> Result<ChatTurnOutcome, String> {
    let ChatTurnModelScope {
        turn_id,
        bound_run_id,
        driver_execution_context,
        origin,
        transcript_visibility,
    } = scope;
    // Single authoritative merge: the turn preserves user-authored text (or a
    // long-text artifact reference). Dynamic mode policy is projected
    // separately by EkoContextProjector and never enters this Message.
    let msg: Message = turn.to_message().map_err(|e| {
        tracing::error!(error = %e, "failed to build user message from prepared turn");
        format!("failed to build user message: {e}")
    })?;
    let cancel = res.cancel.clone();
    let sink: std::sync::Arc<dyn ChatSink> = res.sink.clone();
    // P1.1: capture interaction mode before `res` is moved into the chat
    // resources scope, so we can apply Chat-mode tool hiding after acquiring
    // the agent read guard.
    let interaction_mode = res.interaction_mode;
    let _turn_projection_registration = crate::turn_context::turn_prompt_context_registry()
        .register(turn_id.clone(), interaction_mode, origin, turn.authorship);
    let conversation_id = res.conv_id.clone();
    let webhook_emitter = res.webhook_emitter.clone();
    let runtime_store = res.store.clone();
    // Task mode creates its run before the model call, so its product events
    // carry that real run identity. Chat/Auto turns remain run-less until a
    // task tool actually bootstraps a run; ToolContext can derive the same
    // deterministic scope from turn_id without falsely labelling ordinary chat.
    let active_run_id = bound_run_id.or_else(|| {
        (interaction_mode == crate::tasks::task_runtime::InteractionMode::Task)
            .then(|| crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&turn_id))
    });
    // Scope the chat resources into a task_local so tools the agent calls
    // mid-ReAct (create_complex_task / check_run_status / cancel_run, Phase B3)
    // can reach pool/store/sink via `current_chat_resources()`.
    crate::chat_resources::with_chat_resources(res, async move {
        // The RwLock read guard is held for the stream's lifetime because the
        // stream borrows the agent (same pattern as the GUI's normal chat path).
        let inner = agent.inner().clone();
        let guard = inner.read().await;
        let mut webhook_observer =
            WebhookTurnObserver::new(webhook_emitter, guard.model_name().to_string());
        // Tool visibility is invocation-scoped, so pooled agents keep one
        // registry while each interaction mode gets its own product surface.
        let disabled_tools = Some(crate::tool_exposure::disabled_tools_for_mode(
            interaction_mode,
        ));
        let visible_tools =
            crate::tool_exposure::initial_visible_tools(interaction_mode, &guard.tool_names());
        crate::tool_exposure::record_mode_schema_budget(
            interaction_mode,
            &guard.tool_definitions(),
            &visible_tools,
        );
        let visible_tools = Some(visible_tools);

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
        let invocation = echo_core::agent::AgentInvocationContext {
            history: None,
            runtime: Some(echo_core::tools::ExternalRunContext {
                conversation_id,
                // A real pre-created Task-mode run is value-carried across
                // framework spawns. Chat/Auto task tools derive their prospective
                // scope from turn_id and create it only when invoked.
                run_id: active_run_id.clone(),
                turn_id: Some(turn_id.clone()),
                execution_id: driver_execution_context,
                isolation_id: None,
                message_id: Some(turn_id.clone()),
                cancel: Some(std::sync::Arc::new(cancel.clone())),
                trace_sink: Some(framework_trace_sink_for(&sink)),
                delegation_policy: None,
            }),
            working_dir: None,
            cancel: None,
            disabled_tools,
            visible_tools,
            run_budget: None,
        };
        let stream_result = guard
            .execute_stream_message_with_invocation_context(msg, cancel, invocation)
            .await;
        let raw_stream = match stream_result {
            Ok(stream) => stream,
            Err(e) => {
                // F1-5: 此前只 return Err 字符串, 不经 sink 发 Error 事件 →
                // 前端 assistant 消息卡在 streaming。发 Error 让前端终止流式状态。
                tracing::warn!(error = %e, "agent stream setup failed during chat");
                if let Some(emitter) = webhook_observer.emitter.as_ref() {
                    emitter.emit(crate::webhook::WebhookEvent::AgentError {
                        error: e.to_string(),
                    });
                }
                match EventEnvelope::new(
                    &event_identity,
                    1,
                    None,
                    AgentEvent::from_error("chat_driver", &e),
                ) {
                    Ok(event) => {
                        let _ = sink.on_event(ChatDriverEvent::Agent(Box::new(event)));
                    }
                    Err(envelope_error) => {
                        tracing::error!(
                            error = %envelope_error,
                            "failed to construct terminal chat event"
                        );
                    }
                }
                return Err(e.to_string());
            }
        };
        let mut stream = envelope_event_stream(raw_stream, event_identity);
        async {
            let mut terminal_outcome = None;
            let mut expose_internal_synthesis = transcript_visibility == TurnVisibility::Visible;
            while let Some(event_result) = stream.next().await {
                match event_result {
                    Ok(event) => {
                        let event_outcome = TurnOutcome::from_agent_event(&event.payload);
                        if event_outcome.is_some() {
                            terminal_outcome = event_outcome;
                        }
                        if let Some(run_id) = active_run_id.as_deref() {
                            match &event.payload {
                                AgentEvent::LlmUsage {
                                    prompt_tokens,
                                    completion_tokens,
                                    ..
                                } => {
                                    if let Some(store) = runtime_store.as_ref() {
                                        let input_tokens =
                                            u64::try_from(*prompt_tokens).unwrap_or(u64::MAX);
                                        let output_tokens =
                                            u64::try_from(*completion_tokens).unwrap_or(u64::MAX);
                                        match store.account_run_turn_usage(
                                            run_id,
                                            &turn_id,
                                            event.event_id.as_str(),
                                            input_tokens,
                                            output_tokens,
                                        ) {
                                            Ok(true) => {
                                                if let Err(error) = store.request_pause_with_reason(
                                                    run_id,
                                                    crate::tasks::task_runtime::RunPauseReason::TokenBudget,
                                                    Some("the configured token budget was reached at a provider usage boundary"),
                                                ) {
                                                    tracing::warn!(
                                                        %error,
                                                        run_id,
                                                        "failed to pause a token-budget-exhausted run"
                                                    );
                                                }
                                            }
                                            Ok(false) => {}
                                            Err(error) => tracing::warn!(
                                                %error,
                                                run_id,
                                                "failed to account RunTurn usage"
                                            ),
                                        }
                                    }
                                }
                                AgentEvent::ContextCompressed { .. } => {
                                    if let Some(store) = runtime_store.as_ref()
                                        && let Err(error) = store.record_run_turn_compaction(
                                            run_id,
                                            &turn_id,
                                            event.event_id.as_str(),
                                        )
                                    {
                                        tracing::warn!(
                                            %error,
                                            run_id,
                                            "failed to persist RunTurn compaction"
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                        webhook_observer.observe(&event);
                        if !expose_internal_synthesis
                            && matches!(
                                &event.payload,
                                AgentEvent::ToolResult { name, .. } if name == "task_execute"
                            )
                        {
                            expose_internal_synthesis = active_run_id
                                .as_deref()
                                .zip(runtime_store.as_deref())
                                .and_then(|(run_id, store)| store.get_run(run_id).ok().flatten())
                                .is_some_and(|run| {
                                    run.status
                                        == crate::tasks::task_runtime::TaskRunStatus::Completed
                                });
                        }
                        if !expose_internal_synthesis
                            && matches!(&event.payload, AgentEvent::FinalAnswer(_))
                        {
                            expose_internal_synthesis = active_run_id
                                .as_deref()
                                .zip(runtime_store.as_deref())
                                .and_then(|(run_id, store)| store.get_run(run_id).ok().flatten())
                                .is_some_and(|run| {
                                    run.status
                                        == crate::tasks::task_runtime::TaskRunStatus::Completed
                                });
                        }
                        let suppress_internal_transcript = !expose_internal_synthesis
                            && matches!(
                                &event.payload,
                                AgentEvent::Token(_)
                                    | AgentEvent::ThinkStart
                                    | AgentEvent::ThinkEnd { .. }
                                    | AgentEvent::FinalAnswer(_)
                            );
                        if !suppress_internal_transcript
                            && !sink.on_event(ChatDriverEvent::Agent(Box::new(event)))
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        // The envelope adapter normalizes raw stream errors into
                        // terminal payloads. This branch remains for future
                        // transport adapters that can fail independently.
                        tracing::warn!(error = %e, "agent stream error during chat");
                        let error = e.to_string();
                        if let Some(emitter) = webhook_observer.emitter.as_ref() {
                            emitter.emit(crate::webhook::WebhookEvent::AgentError {
                                error: error.clone(),
                            });
                        }
                        return Err(error);
                    }
                }
            }
            let terminal = terminal_outcome.unwrap_or_else(|| {
                TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "chat_driver",
                    "chat event consumer closed before the terminal event",
                ))
            });
            Ok::<ChatTurnOutcome, String>(webhook_observer.finish(turn_id, terminal))
        }
        .await
    })
    .await
}

/// A `ChatSink` that forwards every product event to an mpsc channel.
///
/// Used by modes whose renderer consumes a channel and applies a
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

    /// Build a minimal [`PreparedUserTurn`] for tests that do not exercise
    /// spill or attachment logic.
    fn make_turn(text: &str) -> crate::prepared_turn::PreparedUserTurn {
        crate::prepared_turn::PreparedUserTurn {
            instruction: text.to_string(),
            resources: vec![],
            authorship: crate::prepared_turn::InstructionAuthorship::User,
        }
    }

    #[test]
    fn task_mode_uses_only_task_runtime_dispatch_tools() {
        let disabled = crate::tool_exposure::disabled_tools_for_mode(
            crate::tasks::task_runtime::InteractionMode::Task,
        );
        assert!(disabled.contains("agent_tool"));
        assert!(disabled.contains("create_complex_task"));
        // Background command cells are shared execution primitives. The thin
        // watch_cell surface may dispatch only the dedicated awaiter and does
        // not create a second TaskRuntime task relation.
        assert!(!disabled.contains("wait"));
        assert!(!disabled.contains("stop_cell"));
        assert!(!disabled.contains("list_cells"));
        assert!(!disabled.contains("watch_cell"));
        assert!(!disabled.contains("task_create"));
        assert!(!disabled.contains("task_execute"));
    }

    #[test]
    fn auto_mode_requires_task_runtime_for_delegation() {
        let disabled = crate::tool_exposure::disabled_tools_for_mode(
            crate::tasks::task_runtime::InteractionMode::Auto,
        );
        assert!(disabled.contains("agent_tool"));
        assert!(!disabled.contains("task_execute"));
        assert!(!disabled.contains("create_complex_task"));
        assert!(!disabled.contains("wait"));
        assert!(!disabled.contains("stop_cell"));
        assert!(!disabled.contains("list_cells"));
        assert!(!disabled.contains("watch_cell"));
    }

    #[test]
    fn chat_mode_exposes_the_same_task_graph_api() {
        let disabled = crate::tool_exposure::disabled_tools_for_mode(
            crate::tasks::task_runtime::InteractionMode::Chat,
        );
        assert!(!disabled.contains("task_create"));
        assert!(!disabled.contains("task_update"));
        assert!(!disabled.contains("task_list"));
        assert!(!disabled.contains("task_execute"));
        assert!(disabled.contains("create_complex_task"));
    }

    struct CountingTool {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl echo_core::tools::Tool for CountingTool {
        fn name(&self) -> &str {
            "web_fetch"
        }

        fn description(&self) -> &str {
            "test invocation-scoped tool visibility"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        fn execute<'a>(
            &'a self,
            _parameters: echo_core::tools::ToolParameters,
        ) -> futures::future::BoxFuture<'a, echo_core::error::Result<echo_core::tools::ToolResult>>
        {
            Box::pin(async move {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(echo_core::tools::ToolResult::success("created"))
            })
        }
    }

    /// Test-only sink that records received events for assertions.
    struct MockChatSink {
        events: std::sync::Mutex<Vec<EventEnvelope>>,
        execution_paths: std::sync::Mutex<Vec<(String, String)>>,
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
                ChatDriverEvent::ExecutionPath {
                    requested_mode,
                    observed_path,
                } => self
                    .execution_paths
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push((requested_mode, observed_path)),
                ChatDriverEvent::Execution(_)
                | ChatDriverEvent::TurnStatus { .. }
                | ChatDriverEvent::Interrupt { .. } => {}
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
        fn final_answer_count(&self) -> usize {
            self.events
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .filter(|event| matches!(event.payload, AgentEvent::FinalAnswer(_)))
                .count()
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

        fn execution_paths(&self) -> Vec<(String, String)> {
            self.execution_paths
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        }
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
            format!(
                "timed out waiting for {run_id} to become {}",
                expected.as_str()
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
            pool: None,
            store: None,
            sink: std::sync::Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("pinned-memory-conversation".to_string()),
            root_message_id: "pinned-memory-turn".to_string(),
            attachments: Vec::new(),
            cancel: echo_agent::agent::CancellationToken::new(),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Auto,
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
        use echo_agent::agent::{CancellationToken, ReactAgentBuilder};
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
            pool: Some(pool.clone()),
            store: Some(store),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("pooled-order-conversation".to_string()),
            root_message_id: "pooled-order-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Chat,
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
    async fn pool_admission_failure_pauses_claimed_continuation_instead_of_failing_goal()
    -> Result<(), String> {
        use echo_agent::agent::{CancellationToken, ReactAgentBuilder};
        use echo_agent::testing::MockLlmClient;
        use std::sync::Arc;

        let agent = AgentHandle::new(
            ReactAgentBuilder::new()
                .model("closed-pool")
                .llm_client(Arc::new(
                    MockLlmClient::new().with_model_name("closed-pool"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let pool =
            Arc::new(crate::agent_pool::AgentPool::new_for_test(agent, None, None, 3, false).await);
        pool.shutdown().await?;
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let resources = Arc::new(crate::chat_resources::ChatResources {
            pool: Some(pool.clone()),
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("closed-pool-conversation".to_string()),
            root_message_id: "closed-pool-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Task,
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let error = drive_pooled_chat(
            pool,
            "__continuation__:closed-pool-run",
            |_| async { Ok(()) },
            &make_turn("continue safely"),
            resources,
        )
        .await
        .err()
        .ok_or_else(|| "closed pool unexpectedly admitted continuation".to_string())?;
        assert!(error.contains("AgentPool admission failed"));

        let run_id =
            crate::tasks::task_runtime::task_tools::formal_run_id_for_turn("closed-pool-turn");
        let snapshot = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "claimed continuation state missing".to_string())?;
        assert_eq!(
            snapshot.run.status,
            crate::tasks::task_runtime::TaskRunStatus::Paused
        );
        let continuation = snapshot
            .continuation
            .ok_or_else(|| "continuation projection missing".to_string())?;
        assert!(continuation.active_turn.is_none());
        assert_eq!(
            continuation.pause.map(|pause| pause.reason),
            Some(crate::tasks::task_runtime::RunPauseReason::ProviderUnavailable)
        );
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
            pool: Some(pool.clone()),
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("pre-driver-cancel-conversation".to_string()),
            root_message_id: "pre-driver-cancel-turn".to_string(),
            attachments: Vec::new(),
            cancel,
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Task,
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
            pool: Some(pool.clone()),
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("pre-driver-shutdown-conversation".to_string()),
            root_message_id: "pre-driver-shutdown-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Task,
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
                pool: None,
                store: Some(store.clone()),
                sink: Arc::new(MockChatSink::default()),
                webhook_emitter: None,
                conv_id: Some("claim-race-conversation".to_string()),
                root_message_id: turn_id.to_string(),
                attachments: Vec::new(),
                cancel: CancellationToken::new(),
                interaction_mode: crate::tasks::task_runtime::InteractionMode::Task,
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

    #[tokio::test]
    async fn shutdown_during_active_continuation_turn_is_boot_resumable() -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;

        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("shutdown-continuation")
                .with_delay(std::time::Duration::from_secs(5))
                .with_response("late response"),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("shutdown-continuation")
                .llm_client(llm)
                .build()
                .map_err(|error| error.to_string())?,
        );
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let resources = Arc::new(crate::chat_resources::ChatResources {
            pool: None,
            store: Some(store.clone()),
            sink: Arc::new(MockChatSink::default()),
            webhook_emitter: None,
            conv_id: Some("shutdown-conversation".to_string()),
            root_message_id: "shutdown-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Task,
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let driven_agent = agent.clone();
        let driven = tokio::spawn(async move {
            drive_chat(&driven_agent, &make_turn("survive shutdown"), resources).await
        });
        let run_id =
            crate::tasks::task_runtime::task_tools::formal_run_id_for_turn("shutdown-turn");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if store
                    .get_run_state(&run_id)
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
        .map_err(|_| "continuation turn did not become active".to_string())?;
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        let _outcome = driven
            .await
            .map_err(|error| format!("driven task failed: {error}"))?;
        let snapshot = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "shutdown run state missing".to_string())?;
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
    async fn task_mode_creates_formal_run_and_rejects_direct_fallback() -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;

        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("t")
                .with_delay(std::time::Duration::from_millis(250))
                .with_responses([
                    "direct answer without plan",
                    "still no plan",
                    "still no task progress",
                ]),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(llm.clone())
                .build()
                .map_err(|error| error.to_string())?,
        );
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let chat_sink = Arc::new(MockChatSink::default());
        let resources = Arc::new(crate::chat_resources::ChatResources {
            pool: None,
            store: Some(store.clone()),
            sink: chat_sink.clone(),
            webhook_emitter: None,
            conv_id: Some("task-conversation".to_string()),
            root_message_id: "task-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Task,
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });

        drive_chat(&agent, &make_turn("build a formal plan"), resources).await?;

        let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn("task-turn");
        let immediate = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                "formal task run missing after the finite foreground turn".to_string()
            })?;
        assert_eq!(
            immediate.status,
            crate::tasks::task_runtime::TaskRunStatus::Running
        );
        assert_eq!(immediate.goal, "build a formal plan");
        assert!(llm.call_count() < 3);
        let run = wait_for_run_status(
            &store,
            &run_id,
            crate::tasks::task_runtime::TaskRunStatus::Paused,
        )
        .await?;
        assert_eq!(run.route, "agent_task_plan");
        assert_eq!(llm.call_count(), 3);
        assert_eq!(chat_sink.final_answer_count(), 1);
        let continuation = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "continuation projection missing".to_string())?;
        assert_eq!(continuation.next_turn_ordinal, 4);
        assert_eq!(
            continuation.last_turn.as_ref().map(|turn| turn.ordinal),
            Some(3)
        );
        assert_eq!(
            continuation.pause.as_ref().map(|pause| pause.reason),
            Some(crate::tasks::task_runtime::RunPauseReason::RepeatedBlocker)
        );
        assert_eq!(
            continuation
                .blocker_audit
                .as_ref()
                .map(|audit| audit.consecutive_turns),
            Some(3)
        );
        assert_eq!(
            chat_sink.execution_paths(),
            vec![
                ("task".to_string(), "formal_plan".to_string()),
                ("task".to_string(), "formal_plan".to_string()),
                ("task".to_string(), "formal_plan".to_string()),
            ]
        );
        let has_path_event = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?
            .iter()
            .any(|event| {
                event.payload.get("kind").and_then(|value| value.as_str()) == Some("execution_path")
                    && event
                        .payload
                        .get("requested_mode")
                        .and_then(|value| value.as_str())
                        == Some("task")
            });
        assert!(has_path_event);
        Ok(())
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
        let sink = Arc::new(MockChatSink::default());
        let resources = Arc::new(crate::chat_resources::ChatResources {
            pool: None,
            store: Some(store.clone()),
            sink,
            webhook_emitter: None,
            conv_id: Some("resume-conversation".to_string()),
            root_message_id: "surface-resume-message".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Task,
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        drive_chat_turn(
            &agent,
            &make_turn("continue the existing goal"),
            resources,
            Some(RunTurnBinding {
                run_id: Some("existing-goal".to_string()),
                turn_id: "resume-turn".to_string(),
                root_message_id: "root-message".to_string(),
                origin: RunTurnOrigin::Resume,
                transcript_visibility: TurnVisibility::Visible,
            }),
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

        let auto_agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("auto-resume-test")
                .llm_client(Arc::new(
                    echo_agent::testing::MockLlmClient::new()
                        .with_model_name("auto-resume-test")
                        .with_responses(["auto pass one", "auto pass two", "auto pass three"]),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        drive_chat(
            &auto_agent,
            &make_turn("continue"),
            Arc::new(crate::chat_resources::ChatResources {
                pool: None,
                store: Some(store.clone()),
                sink: Arc::new(MockChatSink::default()),
                webhook_emitter: None,
                conv_id: Some("resume-conversation".to_string()),
                root_message_id: "auto-resume-message".to_string(),
                attachments: Vec::new(),
                cancel: CancellationToken::new(),
                interaction_mode: crate::tasks::task_runtime::InteractionMode::Auto,
                review_integration: None,
                layer_manager: None,
                memory_generation: None,
                human_loop_provider: None,
            }),
        )
        .await?;
        wait_for_run_status(
            &store,
            "existing-goal",
            crate::tasks::task_runtime::TaskRunStatus::Paused,
        )
        .await?;
        let resumed_starts = store
            .list_events("existing-goal", 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| event.event_type == RuntimeEventKind::RunTurnStarted)
            .collect::<Vec<_>>();
        assert_eq!(resumed_starts.len(), 6);
        assert_eq!(
            resumed_starts
                .get(3)
                .and_then(|event| event.payload.get("turn_id"))
                .and_then(serde_json::Value::as_str),
            Some("auto-resume-message")
        );
        assert_eq!(
            resumed_starts
                .get(3)
                .and_then(|event| event.payload.get("origin"))
                .and_then(serde_json::Value::as_str),
            Some("resume")
        );
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

    async fn drive_scripted_task_graph(
        mode: crate::tasks::task_runtime::InteractionMode,
        turn_id: &str,
    ) -> Result<(), String> {
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
            pool: None,
            store: Some(store.clone()),
            sink,
            webhook_emitter: None,
            conv_id: Some(format!("{turn_id}-conversation")),
            root_message_id: turn_id.to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            interaction_mode: mode,
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
                    "{mode:?} task_create did not create TaskRun {run_id} after {} LLM calls; observed runs: {observed_runs:?}; last request: {:?}",
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
            return Err(format!("{mode:?} task_execute left run {:?}", run.status));
        }
        let plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("{mode:?} task_create did not persist a plan"))?;
        assert_eq!(plan.run_id, run_id);
        assert_eq!(plan.tasks.len(), 1);
        let task_status = plan.tasks.first().map(|task| task.status);
        assert_ne!(
            task_status,
            Some(crate::tasks::task_runtime::TodoStatus::Pending),
            "{mode:?} task_execute must advance the task created under outer run {run_id}"
        );
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(store.active_run_driver_count()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn chat_task_create_and_execute_use_the_canonical_turn_driver() -> Result<(), String> {
        drive_scripted_task_graph(
            crate::tasks::task_runtime::InteractionMode::Chat,
            "chat-task-graph",
        )
        .await
    }

    #[tokio::test]
    async fn auto_task_create_and_execute_use_the_canonical_turn_driver() -> Result<(), String> {
        drive_scripted_task_graph(
            crate::tasks::task_runtime::InteractionMode::Auto,
            "auto-task-graph",
        )
        .await
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
            pool: Some(pool.clone()),
            store: Some(store.clone()),
            sink,
            webhook_emitter: None,
            conv_id: Some("pool-conversation".to_string()),
            root_message_id: "pool-task-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Auto,
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
    async fn chat_and_auto_admission_rejection_leave_taskruntime_unmodified() -> Result<(), String>
    {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;

        for (mode, workspace_transition) in [
            (crate::tasks::task_runtime::InteractionMode::Chat, false),
            (crate::tasks::task_runtime::InteractionMode::Auto, true),
        ] {
            let store = Arc::new(
                crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                    .map_err(|error| error.to_string())?,
            );
            let workspace_transition_guard = if workspace_transition {
                Some(
                    store
                        .begin_workspace_transition()
                        .await
                        .map_err(|error| error.to_string())?,
                )
            } else {
                store
                    .shutdown_run_drivers()
                    .await
                    .map_err(|error| error.to_string())?;
                None
            };
            let agent = AgentHandle::new(
                echo_agent::agent::ReactAgentBuilder::new()
                    .llm_client(Arc::new(
                        echo_agent::testing::MockLlmClient::new().with_response("unused"),
                    ))
                    .build()
                    .map_err(|error| error.to_string())?,
            );
            let turn_id = format!("{}-rejected", mode.as_str());
            let resources = Arc::new(crate::chat_resources::ChatResources {
                pool: None,
                store: Some(store.clone()),
                sink: Arc::new(MockChatSink::default()),
                webhook_emitter: None,
                conv_id: None,
                root_message_id: turn_id.clone(),
                attachments: Vec::new(),
                cancel: CancellationToken::new(),
                interaction_mode: mode,
                review_integration: None,
                layer_manager: None,
                memory_generation: None,
                human_loop_provider: None,
            });
            assert!(
                drive_chat(&agent, &make_turn("must not execute"), resources)
                    .await
                    .is_err()
            );
            drop(workspace_transition_guard);
            let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&turn_id);
            assert!(
                store
                    .get_run(&run_id)
                    .map_err(|error| error.to_string())?
                    .is_none()
            );
            assert!(
                store
                    .get_plan(&run_id)
                    .map_err(|error| error.to_string())?
                    .is_none()
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn drive_chat_streams_agent_events_via_sink() {
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
                .expect("test agent should build"),
        );
        let cancel = CancellationToken::new();
        let chat_sink = Arc::new(MockChatSink::default());
        let sink: Arc<dyn ChatSink> = chat_sink.clone();
        let store = Arc::new(
            crate::tasks::task_runtime::store::TaskRuntimeStore::new_in_memory()
                .expect("in-memory store"),
        );
        let res = Arc::new(crate::chat_resources::ChatResources {
            pool: None,
            store: Some(Arc::clone(&store)),
            sink,
            webhook_emitter: None,
            conv_id: Some("c1".to_string()),
            root_message_id: "m1".to_string(),
            attachments: vec![],
            cancel,
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Auto,
            review_integration: None,
            layer_manager: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let outcome = drive_chat(&agent, &make_turn("hi"), res)
            .await
            .expect("drive_chat should succeed");
        assert_eq!(outcome, TurnOutcome::Completed);
        // The agent's FinalAnswer is streamed through the sink.
        assert!(
            chat_sink.has_final_answer(),
            "drive_chat should stream FinalAnswer to sink; events recorded: {}",
            chat_sink.event_count()
        );
        assert!(
            chat_sink.has_valid_contract("c1", "m1"),
            "drive_chat should preserve version, identity, sequence, and exactly one terminal"
        );
        assert!(
            store
                .get_run(&crate::tasks::task_runtime::task_tools::formal_run_id_for_turn("m1"))
                .expect("read task run")
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
            pool: None,
            store: None,
            sink: chat_sink.clone(),
            webhook_emitter: None,
            conv_id: Some("failure-conversation".to_string()),
            root_message_id: "failure-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Auto,
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
                goal: "boundary goal".to_string(),
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
            pool: None,
            store: Some(Arc::clone(&store)),
            sink,
            webhook_emitter: None,
            conv_id: Some("c1".to_string()),
            root_message_id: turn_id.to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Auto,
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
    async fn drive_chat_keeps_user_text_raw_and_projects_mode_contract() -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use echo_agent::compression::is_context_projection_message;
        use echo_core::llm::types::Role;
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
        let res = Arc::new(crate::chat_resources::ChatResources {
            pool: None,
            store: Some(store),
            sink,
            webhook_emitter: None,
            conv_id: None,
            root_message_id: "m1".to_string(),
            attachments: vec![],
            cancel,
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Chat,
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
        if !contract.contains("Interaction mode: Chat")
            || !contract.contains("Instruction author: user")
        {
            return Err(format!("turn contract is incomplete: {contract}"));
        }
        if crate::turn_context::turn_prompt_context_registry().contains("m1") {
            return Err("turn contract registration leaked after the turn".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn chat_tool_exclusions_are_invocation_scoped_on_pooled_agent() -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mock = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("t")
                .then_tool_call("chat-call", "web_fetch", "{}")
                .with_response("chat done")
                .then_tool_call("auto-call", "web_fetch", "{}")
                .with_response("auto done"),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .tool(Box::new(CountingTool {
                    calls: Arc::clone(&calls),
                }))
                .build()
                .map_err(|error| error.to_string())?,
        );

        for (root_message_id, interaction_mode) in [
            (
                "chat-hidden",
                crate::tasks::task_runtime::InteractionMode::Chat,
            ),
            (
                "auto-visible",
                crate::tasks::task_runtime::InteractionMode::Auto,
            ),
        ] {
            let sink: Arc<dyn ChatSink> = Arc::new(MockChatSink::default());
            let resources = Arc::new(crate::chat_resources::ChatResources {
                pool: None,
                store: None,
                sink,
                webhook_emitter: None,
                conv_id: None,
                root_message_id: root_message_id.to_string(),
                attachments: Vec::new(),
                cancel: CancellationToken::new(),
                interaction_mode,
                review_integration: None,
                layer_manager: None,
                memory_generation: None,
                human_loop_provider: None,
            });
            drive_chat(&agent, &make_turn("run"), resources).await?;
        }

        if calls.load(std::sync::atomic::Ordering::SeqCst) != 1 {
            return Err("Chat exclusion leaked or Auto tool execution was blocked".to_string());
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
