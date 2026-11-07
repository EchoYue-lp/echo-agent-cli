//! 应用层 IM channel 消息处理器 —— 把 IM 消息桥接到 `AgentPool`。
//!
//! 框架层 `AgentChannelHandler::from_config` 要求调用方显式传入 `LlmConfig`，而 EKO
//! channel 不直接构造该 handler。此处 agent 从 `AgentPool::acquire` 取，经
//! `AgentRuntime::bootstrap` 全套接通
//! （state_store / store / compressor / MemoryLayerManager / permission_service /
//! cache_user_id / conversation_id）。会话身份与 framework `SessionHandler` 一致，按
//! channel + conversation + sender 隔离；群聊成员不会交叉复用 Agent、TaskRun 或缓存。
//!
//! 归属（spec §D1-6）：`AgentPool` 是 EKO 产品概念，handler 放共享应用层（app-core），
//! 不进框架 `channels.rs`。框架复用方可按需使用要求显式 LLM 依赖的
//! `AgentChannelHandler::from_config` / `from_config_with_client`。

mod management;
#[cfg(feature = "channels")]
mod weixin_control;
pub use management::{
    ChannelConfigurationPatch, ChannelControlAction, ChannelRuntime, ManagedChannel, WeixinQrStatus,
};

#[cfg(test)]
pub(crate) use management::test_support;

#[cfg(feature = "channels")]
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::Arc,
};

#[cfg(feature = "channels")]
use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnError, ForegroundTurnSurface};

#[cfg(feature = "channels")]
use crate::hitl::{ChannelHumanLoopProvider, ChannelHumanLoopResolution};

#[cfg(feature = "channels")]
use sha2::{Digest, Sha256};

#[cfg(feature = "channels")]
use weixin_control::{ControlEffectObserver, ControlEffectOutcome};

#[cfg(feature = "channels")]
async fn report_channel_control(
    control: &Option<ControlEffectObserver>,
    outcome: ControlEffectOutcome,
) {
    if let Some(control) = control {
        control.report(outcome).await;
    }
}

/// A retry is safe only when the original branch proves no business effect.
/// Cancellation attempted before reset admission may already have changed the
/// old turn, so failed reset admission then requires explicit recovery.
#[cfg(feature = "channels")]
fn reset_pre_effect_outcome(cancellation_attempted: bool, reason: &str) -> ControlEffectOutcome {
    if cancellation_attempted {
        ControlEffectOutcome::Unknown {
            reason: reason.to_string(),
        }
    } else {
        ControlEffectOutcome::NotAdmitted {
            reason: reason.to_string(),
        }
    }
}

#[cfg(feature = "channels")]
fn settled_control_outcome(
    effect_completed: bool,
    settlement_confirmed: bool,
    reason: &str,
) -> ControlEffectOutcome {
    if effect_completed && settlement_confirmed {
        ControlEffectOutcome::Applied
    } else {
        ControlEffectOutcome::Unknown {
            reason: reason.to_string(),
        }
    }
}

#[cfg(feature = "channels")]
fn channel_input_has_terminal_fact(
    receipt: &crate::conversation_input::ConversationInputReceipt,
) -> bool {
    use crate::conversation_input::ConversationInputPhase;
    receipt.phase == ConversationInputPhase::Cancelled
        || (receipt.phase == ConversationInputPhase::TurnSettled
            && receipt.drained
            && receipt.outcome.is_some())
}

#[cfg(feature = "channels")]
async fn render_agent_command_result(
    control: &Option<ControlEffectObserver>,
    result: Result<String, crate::text_commands::agent_router::AgentTextCommandError>,
) -> String {
    use crate::text_commands::agent_router::AgentTextCommandError;
    match result {
        Ok(text) => {
            report_channel_control(control, ControlEffectOutcome::Applied).await;
            text
        }
        Err(error) => {
            let effect = match &error {
                AgentTextCommandError::InvalidInput(_) => ControlEffectOutcome::Rejected { reason: "Agent control syntax or group member input was rejected before dispatch".to_string() },
                AgentTextCommandError::Unavailable(_) => ControlEffectOutcome::NotAdmitted { reason: "The original Agent routing service was unavailable before dispatch".to_string() },
                AgentTextCommandError::Operation(_) => ControlEffectOutcome::Unknown { reason: "The original Agent routing owner did not return a confirmed operation result".to_string() },
            };
            report_channel_control(control, effect).await;
            error.to_string()
        }
    }
}

#[cfg(feature = "channels")]
enum ChannelRenderEvent {
    Driver(crate::chat_driver::ChatDriverEvent),
    // Keep the full owned journal envelope without inflating every bounded queue slot.
    Journaled(Box<crate::chat_event_log::ChatEventEnvelope>),
    ToolProjection(crate::tool_execution_projection::ToolExecutionProjectionUpdate),
    Prompt(String),
    Terminal(crate::chat_driver::TurnOutcome),
}

#[cfg(all(test, feature = "channels"))]
fn trace_channel_boundary(boundary: &str) {
    if std::env::var_os("EKO_WEIXIN_A2_PHASE").is_some() {
        eprintln!("A2_OWNER_BOUNDARY:{boundary}");
    }
}

/// Use the existing bounded render queue without blocking the original driver
/// (including its cancellation/GUI winner). Backpressure is not session death.
#[cfg(feature = "channels")]
async fn drive_with_channel_prompts<F: std::future::Future>(
    drive: F,
    mut prompts: tokio::sync::broadcast::Receiver<String>,
    prompt_tx: tokio::sync::mpsc::Sender<ChannelRenderEvent>,
    hitl: &ChannelHumanLoopProvider,
) -> F::Output {
    tokio::pin!(drive);
    let mut prompt_open = true;
    let mut renderer_available = true;
    loop {
        tokio::select! {
            result = &mut drive => return result,
            prompt = prompts.recv(), if prompt_open => match prompt {
                Ok(_) if !renderer_available => {
                    // Continue observing this same drive's later requests.
                    // They must withdraw IM immediately rather than enter an
                    // unobserved FIFO until their deadline. A later turn owns
                    // a fresh renderer and starts with normal availability.
                    hitl.reject_all("Original IM prompt observer is unavailable").await;
                }
                Ok(prompt) => {
                    tokio::select! {
                        result = &mut drive => return result,
                        delivered = prompt_tx.send(ChannelRenderEvent::Prompt(prompt)) => {
                            if delivered.is_err() {
                                // Withdraw only current IM deliveries. A later
                                // turn gets its own renderer on this same session.
                                hitl.reject_all("Original IM prompt observer is unavailable").await;
                                renderer_available = false;
                            }
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "channel prompt observation lagged; current IM requests withdrawn");
                    hitl.reject_all("Original IM prompt observation lagged").await;
                    renderer_available = false;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    hitl.reject_all("Original IM prompt observation closed").await;
                    prompt_open = false;
                }
            }
        }
    }
}

#[cfg(feature = "channels")]
mod outbound;

#[cfg(all(test, feature = "channels"))]
#[path = "channel_runtime/native_message_tests.rs"]
mod native_message_tests;

#[cfg(feature = "channels")]
mod input_pump;

#[cfg(feature = "channels")]
use outbound::{
    CHANNEL_EVENT_QUEUE_CAPACITY, ChannelOutboundDraft, ChannelSurfaceSink,
    channel_outbound_chunks, channel_outbound_transport, channel_safe_text,
    channel_terminal_stream,
};

#[cfg(feature = "channels")]
mod tool_render;

#[cfg(feature = "channels")]
use tool_render::{
    CHANNEL_TOOL_PROGRESS_CHARS, ChannelExecutionToolCompleted, ChannelExecutionToolOutput,
    ChannelExecutionToolStarted, ChannelToolAddress, ChannelToolObserveOutcome, ChannelToolOwner,
    ChannelToolRenderState, ChannelToolTerminal, channel_tool_args_preview,
    channel_tool_result_message, channel_verified_artifact,
};

#[cfg(all(feature = "channels", test))]
use outbound::{
    CHANNEL_OUTBOUND_TOTAL_MESSAGES, ChannelBufferOutcome, ChannelStreamingSanitizer,
    channel_outbound_transport_unpaced, channel_rate_deadline, channel_rate_policy,
};

#[cfg(all(feature = "channels", test))]
use tool_render::{
    CHANNEL_ACTIVE_TOOL_LIMIT, CHANNEL_RECENT_TOOL_TERMINALS, CHANNEL_TOOL_OUTPUT_CHARS,
};

#[cfg(feature = "channels")]
enum ChannelTaskRunControl {
    Reply(String),
    Resume {
        expected: crate::tasks::task_runtime::TaskRunResumeIdentity,
        continuation_enabled: bool,
        runtime: Box<crate::state::ScopedChatRuntime>,
    },
}

#[cfg(feature = "channels")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChannelResumeDispatch {
    Planned(crate::tasks::task_runtime::TaskRunResumeIdentity),
    Continuation(crate::tasks::task_runtime::RunTurnBinding),
}

#[cfg(feature = "channels")]
fn channel_resume_dispatch(
    expected: crate::tasks::task_runtime::TaskRunResumeIdentity,
    continuation_enabled: bool,
    turn_id: &str,
) -> ChannelResumeDispatch {
    if continuation_enabled && expected.uses_conversation_driver() {
        ChannelResumeDispatch::Continuation(
            crate::tasks::task_runtime::RunTurnBinding::resume_expected(expected, turn_id),
        )
    } else {
        ChannelResumeDispatch::Planned(expected)
    }
}

#[cfg(feature = "channels")]
fn channel_resume_rejects_attachments(is_resume: bool, attachment_count: usize) -> bool {
    is_resume && attachment_count > 0
}

#[cfg(feature = "channels")]
#[derive(Clone)]
struct ChannelActiveTurn {
    runtime: crate::state::ScopedChatRuntime,
    agent_conversation_id: String,
    conversation_id: String,
    turn_id: String,
}

#[cfg(feature = "channels")]
fn channel_input_address(
    workspace_id: &str,
    conversation_id: &str,
) -> crate::conversation_input::ConversationInputAddress {
    crate::conversation_input::ConversationInputAddress {
        workspace_id: workspace_id.to_string(),
        conversation_id: conversation_id.to_string(),
    }
}

#[cfg(feature = "channels")]
fn channel_input_effective_text(
    payload: &crate::conversation_input::ConversationInputPayload,
) -> String {
    use crate::conversation_input::ChannelExecutionIntent;
    match payload
        .channel_binding
        .as_ref()
        .map(|binding| &binding.execution)
    {
        Some(ChannelExecutionIntent::Resume { expected, .. }) => format!(
            "Resume the existing TaskRun {} toward its unchanged Goal. Reload the authoritative TaskRuntime projection and continue the next useful work.",
            expected.run_id
        ),
        Some(ChannelExecutionIntent::Chat)
            if payload.text.split_whitespace().next() == Some("/steer") =>
        {
            payload
                .text
                .trim()
                .strip_prefix("/steer")
                .map(str::trim)
                .unwrap_or_default()
                .to_string()
        }
        _ => payload.text.clone(),
    }
}

#[cfg(feature = "channels")]
fn channel_input_attempt(
    projection: &crate::conversation_input::ConversationInputProjection,
) -> Result<crate::conversation_input::ConversationInputAttempt, String> {
    projection
        .active_attempt
        .clone()
        .ok_or_else(|| "channel input active attempt is missing".to_string())
}

#[cfg(feature = "channels")]
fn channel_input_phase_label(
    phase: crate::conversation_input::ConversationInputPhase,
) -> &'static str {
    use crate::conversation_input::ConversationInputPhase;

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

#[cfg(feature = "channels")]
async fn settle_channel_turn_after_input_observers(
    lease: crate::foreground_turn::ForegroundTurnLease,
    outcome: crate::chat_driver::TurnOutcome,
) -> Result<crate::chat_driver::TurnOutcome, String> {
    lease
        .settle_after_observers(outcome)
        .await
        .map(|settlement| settlement.outcome)
        .map_err(|error| error.to_string())
}

#[cfg(feature = "channels")]
fn channel_live_terminal_projector(
    service: crate::conversation_input::ConversationInputService,
    attempt: crate::conversation_input::ConversationInputAttempt,
    observed_phase: Arc<
        std::sync::Mutex<Option<crate::conversation_input::ConversationInputPhase>>,
    >,
) -> crate::foreground_turn::ForegroundTerminalProjector {
    use crate::conversation_input::ConversationInputPhase;

    Arc::new(move |outcome| {
        let service = service.clone();
        let attempt = attempt.clone();
        let phase = observed_phase
            .lock()
            .map(|phase| *phase)
            .map_err(|_| "channel live input phase is unavailable".to_string());
        Box::pin(async move {
            let phase = phase?;
            match phase {
                Some(
                    ConversationInputPhase::Drained
                    | ConversationInputPhase::TurnSettled
                    | ConversationInputPhase::RecoveryRequired,
                ) => service
                    .settle_attempt(&attempt, &outcome)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
                None if attempt.observation.failed() => service
                    .settle_attempt(&attempt, &outcome)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
                _ => Ok(()),
            }
        })
    })
}

#[cfg(feature = "channels")]
async fn project_channel_committed_events(
    log: &crate::chat_event_log::ChatEventLog,
    identity: &crate::conversation_input::ConversationInputIdentity,
    render_tx: &tokio::sync::mpsc::Sender<ChannelRenderEvent>,
    cursor: &std::sync::Mutex<u64>,
) -> Result<(), String> {
    let after_cursor = *cursor
        .lock()
        .map_err(|_| "channel input lifecycle cursor is unavailable".to_string())?;
    let replay = log
        .replay(
            &identity.address.workspace_id,
            Some(&identity.address.conversation_id),
            &identity.input_id,
            after_cursor,
        )
        .map_err(|error| error.to_string())?;
    if replay.truncated {
        return Err(format!(
            "channel committed-event replay for {} was truncated after cursor {}",
            identity.input_id, after_cursor
        ));
    }
    for envelope in replay.events {
        let sequence = envelope.sequence;
        if matches!(
            &envelope.payload,
            crate::chat_driver::ChatDriverEvent::InputLifecycle(_)
                | crate::chat_driver::ChatDriverEvent::Execution(_)
        ) {
            render_tx
                .send(ChannelRenderEvent::Driver(envelope.payload))
                .await
                .map_err(|_| "channel committed-event renderer is closed".to_string())?;
        }
        *cursor
            .lock()
            .map_err(|_| "channel input lifecycle cursor is unavailable".to_string())? = sequence;
    }
    Ok(())
}

#[cfg(feature = "channels")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ChannelSurfaceIdentity {
    channel_id: String,
    chat_id: String,
    sender_id: String,
}

#[cfg(feature = "channels")]
type ChannelActiveTurnMap =
    Arc<std::sync::Mutex<HashMap<ChannelSurfaceIdentity, ChannelActiveTurn>>>;

#[cfg(feature = "channels")]
fn publish_channel_active_turn(
    active_turns: &ChannelActiveTurnMap,
    surface_id: &ChannelSurfaceIdentity,
    conversation_id: &str,
    agent_conversation_id: &str,
    runtime: &crate::state::ScopedChatRuntime,
    turn_id: &str,
) -> Result<(), String> {
    let mut active = active_turns
        .lock()
        .map_err(|_| "channel active owner is unavailable".to_string())?;
    if let Some(existing) = active.get(surface_id) {
        return Err(format!("Turn {} is still running.", existing.turn_id));
    }
    active.insert(
        surface_id.clone(),
        ChannelActiveTurn {
            runtime: runtime.clone(),
            agent_conversation_id: agent_conversation_id.to_string(),
            conversation_id: conversation_id.to_string(),
            turn_id: turn_id.to_string(),
        },
    );
    Ok(())
}

#[cfg(feature = "channels")]
fn channel_hitl_scope_resolver(
    active: &ChannelActiveTurnMap,
    foreground: &ForegroundTurnControl,
    surface_id: ChannelSurfaceIdentity,
    agent_conversation_id: String,
    base: crate::hitl::HitlRequestScope,
) -> crate::hitl::HitlScopeResolver {
    let active = Arc::downgrade(active);
    let foreground = foreground.downgrade();
    Arc::new(move || {
        let unavailable = || {
            echo_agent::error::ReactError::Other(
                "Channel HITL original owner is unavailable".to_string(),
            )
        };
        let active_map = active.upgrade().ok_or_else(unavailable)?;
        // Drop the map guard before consulting the original foreground control.
        let owner = active_map
            .lock()
            .map_err(|_| unavailable())?
            .get(&surface_id)
            .cloned()
            .ok_or_else(unavailable)?;
        if owner.agent_conversation_id != agent_conversation_id {
            return Err(unavailable());
        }
        let mut scope = base.clone();
        scope.workspace_id = Some(owner.runtime.execution_scope().workspace_id().to_string());
        scope.host_generation = Some(
            owner
                .runtime
                .workspace_io_receipt()
                .host_generation()
                .to_string(),
        );
        scope.conversation_id = Some(owner.conversation_id);
        scope.root_turn_id = Some(owner.turn_id);
        scope.for_foreground_observer(&foreground, ForegroundTurnSurface::Channel)
    })
}

#[cfg(feature = "channels")]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ChannelRuntimeOwnerKey {
    workspace_id: String,
    workspace_generation: String,
    runtime_state_id: String,
}

#[cfg(feature = "channels")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChannelRetirementPhase {
    Active,
    RetirePending,
    GcPending,
}

#[cfg(feature = "channels")]
#[derive(Clone, Debug)]
struct ChannelRuntimeObligation {
    key: ChannelRuntimeOwnerKey,
    product_conversation_id: String,
    incarnation_id: String,
    phase: ChannelRetirementPhase,
}

#[cfg(feature = "channels")]
type ChannelSessionRetirementGate =
    Arc<std::sync::Mutex<BTreeMap<ChannelRuntimeOwnerKey, ChannelRuntimeObligation>>>;

#[cfg(feature = "channels")]
#[derive(Default)]
struct ChannelSessionLifecycleRecord {
    current_incarnation_id: Option<String>,
    pending_ended_incarnation_id: Option<String>,
    retirement_gate: ChannelSessionRetirementGate,
}

#[derive(Clone)]
struct ChannelPumpSettlement {
    failures: Vec<String>,
}
#[derive(Default)]
struct ChannelInputPumpTasks {
    shutting_down: bool,
    // Original registry stays borrowed by the drain producer. Its panic must
    // not drop/abort the remaining original JoinSet.
    tasks: Arc<std::sync::Mutex<tokio::task::JoinSet<Result<(), String>>>>,
    joined_failures: Arc<std::sync::Mutex<Vec<String>>>,
    physical_receipt: Arc<std::sync::Mutex<Option<Result<ChannelPumpSettlement, String>>>>,
    settlement: Option<
        futures::future::Shared<
            futures::future::BoxFuture<'static, Result<ChannelPumpSettlement, String>>,
        >,
    >,
}

#[cfg(feature = "channels")]
#[derive(Default)]
struct ChannelRecoveryClaims {
    transport_epoch: Option<u64>,
    identities: HashSet<crate::conversation_input::ConversationInputIdentity>,
}

#[cfg(feature = "channels")]
#[derive(Default)]
pub(crate) struct ChannelSessionCoordinator {
    ingress_service_cancel: std::sync::Mutex<Option<tokio_util::sync::CancellationToken>>,
    active_turns: ChannelActiveTurnMap,
    lifecycle: std::sync::Mutex<HashMap<ChannelSurfaceIdentity, ChannelSessionLifecycleRecord>>,
    input_pumps: std::sync::Mutex<
        HashMap<
            ChannelSurfaceIdentity,
            Arc<
                input_pump::ChannelInputPumpSlot<
                    crate::conversation_input::ConversationInputIdentity,
                >,
            >,
        >,
    >,
    input_pump_tasks: std::sync::Mutex<ChannelInputPumpTasks>,
    // Wake handles retained from the original durable handler. They are only
    // a capability to re-enter the handler's own slot; they own no queue,
    // phase or settlement.
    session_wakes: std::sync::Mutex<HashMap<ChannelSurfaceIdentity, Arc<ChannelSessionWake>>>,
    recovery_claims: Arc<std::sync::Mutex<ChannelRecoveryClaims>>,
    lifecycle_retention:
        std::sync::Mutex<std::sync::Weak<crate::runtime::ApplicationLifecycleRetention>>,
}

#[cfg(feature = "channels")]
impl ChannelSessionCoordinator {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn claim_recovery_identity(
        claims: &std::sync::Mutex<ChannelRecoveryClaims>,
        transport_epoch: u64,
        identity: &crate::conversation_input::ConversationInputIdentity,
    ) -> Result<bool, String> {
        let mut claims = claims
            .lock()
            .map_err(|_| "original recovery claim owner unavailable".to_string())?;
        if claims.transport_epoch != Some(transport_epoch) {
            claims.transport_epoch = Some(transport_epoch);
            claims.identities.clear();
        }
        Ok(claims.identities.insert(identity.clone()))
    }

    fn bind_lifecycle_retention(
        &self,
        root: std::sync::Weak<crate::runtime::ApplicationLifecycleRetention>,
    ) {
        *self
            .lifecycle_retention
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = root;
    }

    fn surface_id(
        instance: &echo_agent::channels::ChannelSessionInstance,
    ) -> ChannelSurfaceIdentity {
        ChannelSurfaceIdentity {
            channel_id: instance.channel_id().to_string(),
            chat_id: instance.conversation_id().to_string(),
            sender_id: instance.sender_id().to_string(),
        }
    }

    fn register(
        &self,
        instance: &echo_agent::channels::ChannelSessionInstance,
    ) -> Result<(Option<String>, ChannelSessionRetirementGate), String> {
        let surface_id = Self::surface_id(instance);
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = lifecycle.entry(surface_id.clone()).or_default();
        let explicit_previous = instance.previous_incarnation_id().map(str::to_string);
        let pending_previous = record.pending_ended_incarnation_id.clone();
        let previous = match (explicit_previous, pending_previous) {
            (Some(explicit), Some(pending)) if explicit != pending => {
                return Err(format!(
                    "channel session incarnation conflict: factory replaces {explicit}, callback retained {pending}"
                ));
            }
            (Some(explicit), _) => Some(explicit),
            (None, pending) => pending,
        };
        if let Some(current) = record.current_incarnation_id.as_deref()
            && previous.as_deref() != Some(current)
        {
            return Err(format!(
                "channel session factory replaced live incarnation {current} without an exact predecessor"
            ));
        }
        record.current_incarnation_id = Some(instance.incarnation_id());
        record.pending_ended_incarnation_id = None;
        if let Some(previous) = previous.as_deref() {
            Self::mark_incarnation_pending(&record.retirement_gate, previous);
        }
        Ok((previous, Arc::clone(&record.retirement_gate)))
    }

    fn input_pump(
        &self,
        surface_id: &ChannelSurfaceIdentity,
    ) -> Arc<input_pump::ChannelInputPumpSlot<crate::conversation_input::ConversationInputIdentity>>
    {
        Arc::clone(
            self.input_pumps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(surface_id.clone())
                .or_default(),
        )
    }

    fn retain_session_wake(
        &self,
        surface_id: ChannelSurfaceIdentity,
        wake: Arc<ChannelSessionWake>,
    ) {
        self.session_wakes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(surface_id, wake);
    }

    fn drop_session_wake(&self, surface_id: &ChannelSurfaceIdentity) {
        self.session_wakes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(surface_id);
    }

    /// Find the original handler retained for the exact product conversation.
    /// The conversation id is the same hash used by the journal address, so a
    /// current GUI project or a different account cannot select another owner.
    fn find_session_wake(
        &self,
        channel_id: &str,
        conversation_id: &str,
    ) -> Option<(ChannelSurfaceIdentity, Arc<ChannelSessionWake>)> {
        let wakes = self
            .session_wakes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        wakes.iter().find_map(|(surface, wake)| {
            (surface.channel_id == channel_id
                && AppChannelMessageHandler::conversation_id(
                    &surface.channel_id,
                    &surface.chat_id,
                    &surface.sender_id,
                ) == conversation_id)
                .then(|| (surface.clone(), Arc::clone(wake)))
        })
    }

    pub(crate) fn record_session_end(&self, info: echo_agent::channels::SessionEndInfo) {
        let surface_id = ChannelSurfaceIdentity {
            channel_id: info.channel_id,
            chat_id: info.chat_id,
            sender_id: info.sender_id,
        };
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = lifecycle.entry(surface_id.clone()).or_default();
        let ended_current =
            record.current_incarnation_id.as_deref() == Some(info.incarnation_id.as_str());
        if ended_current {
            record.current_incarnation_id = None;
            Self::mark_incarnation_pending(&record.retirement_gate, &info.incarnation_id);
            record.pending_ended_incarnation_id = Some(info.incarnation_id);
        }
        drop(lifecycle);
        if ended_current {
            self.drop_session_wake(&surface_id);
            if let Some(slot) = self
                .input_pumps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&surface_id)
            {
                let _ = slot.begin_shutdown();
            }
        }
    }

    fn start_channel_recovery_initialization(
        &self,
        state: Arc<crate::state::AppState>,
        handler: Arc<dyn echo_agent::channels::MessageHandler>,
        handle: echo_agent::channels::ChannelHandle,
        account_id: String,
        generation: String,
    ) -> Result<(), String> {
        let cancel = self
            .ingress_service_cancel
            .lock()
            .map_err(|_| "channel service token unavailable".to_string())?
            .clone()
            .ok_or_else(|| "channel service did not bind before recovery".to_string())?;
        let tasks = self
            .input_pump_tasks
            .lock()
            .map_err(|_| "original channel task owner unavailable".to_string())?;
        if tasks.shutting_down || cancel.is_cancelled() {
            return Err("channel service closed before recovery".to_string());
        }
        let recovery_claims = Arc::clone(&self.recovery_claims);
        tasks.tasks.lock().map_err(|_| "original channel JoinSet unavailable".to_string())?.spawn(async move {
            #[cfg(test)]
            trace_channel_boundary("recovery-init.enter");
            let observation = handle.transport_status().await;
            let epoch = observation.mutation_epoch;
            let mut observed = observation.receiver.ok_or_else(|| "original Weixin Ready observation unavailable".to_string())?;
            loop {
                let ready = {
                    let snapshot = observed.borrow_and_update();
                    snapshot.phase == echo_agent::channels::ChannelTransportPhase::Ready
                        && snapshot.evidence == Some(echo_agent::channels::ChannelTransportEvidence::WeixinAuthenticatedPoll)
                };
                if ready { break; }
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(()),
                    changed = observed.changed() => changed.map_err(|_| "original Weixin Ready source closed".to_string())?,
                }
            }
            if cancel.is_cancelled() || handle.management_snapshot().await.mutation_epoch != epoch { return Ok(()); }
            let (candidates, inventory_complete) = state.storage.chat_events.channel_input_bindings().await.map_err(|error| error.to_string())?;
            for input in candidates {
                if cancel.is_cancelled() { return Ok(()); }
                let Some(binding) = input.payload.channel_binding.as_ref() else { continue; };
                if binding.account_id != account_id || binding.credential_generation != generation { continue; }
                let owns_recovery_claim = Self::claim_recovery_identity(
                    &recovery_claims,
                    epoch,
                    &input.receipt.identity,
                )?;
                if !owns_recovery_claim { continue; }
                // Retained terminal DATA exists for provider deduplication, not
                // another recovery attempt. Preserve its original outcome.
                if channel_input_has_terminal_fact(&input.receipt) { continue; }
                // The SAME private admission path distinguishes a current live
                // route from lost RAM and performs the original journal proof.
                // A separate precheck here could mark a freshly started live
                // owner uncertain while the first authenticated poll admits it.
                let target = echo_agent::channels::weixin::encode_target(&binding.account_id, &binding.peer_id).map_err(|error| error.to_string())?;
                let inbound = echo_agent::channels::InboundMessage::new("weixin", target.clone(), &target,
                    echo_agent::channels::ChatType::Direct, input.payload.text.clone(), binding.provider_message_id.clone());
                let (receipt, observed_receipt) = echo_agent::channels::channel_ingress_receipt();
                // The same SessionHandler creates the fresh incarnation/fence.
                // This local observation never advances a provider cursor.
                if let Err(error) = handler.admit_with_ingress(inbound, cancel.child_token(), receipt).await {
                    if cancel.is_cancelled() { return Ok(()); }
                    state.storage.chat_events.mark_channel_input_uncertain_checked(input.receipt.identity.clone(), Some(input.receipt.queue_revision),
                        format!("original channel recovery route was not admitted: {error}")).await.map_err(|error| error.to_string())?;
                }
                drop(observed_receipt);
            }
            if !inventory_complete { return Err("original channel stream discovery isolated uncertain history".to_string()); }
            #[cfg(test)]
            trace_channel_boundary("recovery-init.return-ok");
            Ok(())
        });
        Ok(())
    }

    // Use the existing service JoinSet for native controls which never owned a
    // foreground turn. It owns the operation itself, not an output-only waiter.
    // A completed task is collected into the original shutdown debt before
    // deciding whether a session is idle.
    #[cfg(test)]
    fn has_pending_service_work(&self) -> Result<bool, String> {
        self.reap_completed_service_work()?;
        let tasks = self
            .input_pump_tasks
            .lock()
            .map_err(|_| "original channel service owner unavailable".to_string())?;
        let original = tasks
            .tasks
            .lock()
            .map_err(|_| "original channel JoinSet unavailable".to_string())?;
        Ok(!original.is_empty())
    }

    fn reap_completed_service_work(&self) -> Result<(), String> {
        let tasks = self
            .input_pump_tasks
            .lock()
            .map_err(|_| "original channel service owner unavailable".to_string())?;
        let mut original = tasks
            .tasks
            .lock()
            .map_err(|_| "original channel JoinSet unavailable".to_string())?;
        while let Some(result) = original.try_join_next() {
            let failure = match result {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(error) => Some(error.to_string()),
            };
            if let Some(error) = failure {
                tasks
                    .joined_failures
                    .lock()
                    .map_err(|_| "original channel service receipts unavailable".to_string())?
                    .push(error);
            }
        }
        Ok(())
    }

    fn start_control_task<F>(
        &self,
        observer: ControlEffectObserver,
        operation: F,
    ) -> Result<(), String>
    where
        F: std::future::Future<Output = String> + Send + 'static,
    {
        self.reap_completed_service_work()?;
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
            format!("native channel control requires its original Tokio runtime: {error}")
        })?;
        let cancel = self
            .ingress_service_cancel
            .lock()
            .map_err(|_| "original channel service token unavailable".to_string())?
            .clone()
            .ok_or_else(|| "original channel service is not bound".to_string())?;
        let tasks = self
            .input_pump_tasks
            .lock()
            .map_err(|_| "original channel service owner unavailable".to_string())?;
        if tasks.shutting_down || cancel.is_cancelled() {
            return Err("original channel service is closing".to_string());
        }
        tasks
            .tasks
            .lock()
            .map_err(|_| "original channel JoinSet unavailable".to_string())?
            .spawn_on(
                async move {
                    observer.defer_response();
                    if cancel.is_cancelled() {
                        observer
                            .report(ControlEffectOutcome::NotAdmitted {
                                reason: "Original service closed before the native control started"
                                    .into(),
                            })
                            .await;
                        return Ok(());
                    }
                    // Once polled, the original native owner retains its physical
                    // work through settlement; Stop never drops an accepted effect.
                    let message = operation.await;
                    observer.send_response(message).await;
                    Ok(())
                },
                &runtime,
            );
        Ok(())
    }

    fn start_input_pump_task(
        &self,
        slot: Arc<
            input_pump::ChannelInputPumpSlot<crate::conversation_input::ConversationInputIdentity>,
        >,
        owner: input_pump::ChannelInputPumpOwner<
            crate::conversation_input::ConversationInputIdentity,
        >,
        adapter: Arc<ChannelInputPumpAdapter>,
    ) -> Result<(), String> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| format!("channel input pump requires a Tokio runtime: {error}"))?;
        let tasks = self
            .input_pump_tasks
            .lock()
            .map_err(|_| "channel input pump task owner is unavailable".to_string())?;
        if tasks.shutting_down {
            return Err("channel input pump admission is closed".to_string());
        }
        tasks
            .tasks
            .lock()
            .map_err(|_| "channel pump JoinSet unavailable".to_string())?
            .spawn_on(
                async move {
                    #[cfg(test)]
                    trace_channel_boundary("pump.enter");
                    let mut owner = Some(owner);
                    loop {
                        let current = owner
                            .take()
                            .ok_or_else(|| "channel pump recovery owner is missing".to_string())?;
                        match input_pump::run_channel_input_pump(current, Arc::clone(&adapter))
                            .await
                        {
                            Ok(()) | Err(input_pump::ChannelInputPumpError::ShuttingDown) => {
                                #[cfg(test)]
                                trace_channel_boundary("pump.return");
                                return Ok(());
                            }
                            Err(input_pump::ChannelInputPumpError::DurableDebt(reason)) => {
                                return Err(format!("channel input durable debt: {reason}"));
                            }
                            Err(error) => {
                                tracing::error!(%error, "channel input pump owner failed");
                                owner = slot
                                    .resume_after_owner_loss()
                                    .map_err(|resume| resume.to_string())?;
                                if owner.is_none() {
                                    return Err(error.to_string());
                                }
                            }
                        }
                    }
                },
                &runtime,
            );
        Ok(())
    }

    fn start_post_settlement_input_task(
        &self,
        waiter: crate::foreground_turn::ForegroundTurnSettlementWaiter,
        terminal_tx: tokio::sync::oneshot::Sender<
            Result<crate::chat_driver::TurnOutcome, crate::chat_driver::ChatTurnError>,
        >,
        slot: Arc<
            input_pump::ChannelInputPumpSlot<crate::conversation_input::ConversationInputIdentity>,
        >,
        adapter: Arc<ChannelInputPumpAdapter>,
    ) -> Result<(), String> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| format!("channel input notifier requires a Tokio runtime: {error}"))?;
        let tasks = self
            .input_pump_tasks
            .lock()
            .map_err(|_| "channel input pump task owner is unavailable".to_string())?;
        if tasks.shutting_down {
            return Err("channel input notifier admission is closed".to_string());
        }
        tasks
            .tasks
            .lock()
            .map_err(|_| "channel pump JoinSet unavailable".to_string())?
            .spawn_on(
                async move {
                    let settlement = match waiter.wait().await {
                        Ok(settlement) => settlement,
                        Err(ForegroundTurnError::Observation {
                            run_id,
                            execution_id,
                        }) => {
                            // An observation cannot kick replay or publish a terminal.
                            let _ = terminal_tx.send(Err(
                                crate::chat_driver::ChatTurnError::Observation {
                                    run_id,
                                    execution_id,
                                },
                            ));
                            return Ok(());
                        }
                        Err(error) => return Err(error.to_string()),
                    };
                    let _ = terminal_tx.send(Ok(settlement.outcome));
                    let kick = match slot.kick() {
                        Ok(kick) => Some(kick),
                        Err(input_pump::ChannelInputPumpError::ShuttingDown) => None,
                        Err(error) => return Err(error.to_string()),
                    };
                    if let Some(input_pump::ChannelInputPumpKick::Started(owner)) = kick {
                        let mut owner = Some(owner);
                        loop {
                            let current = owner
                                .take()
                                .ok_or_else(|| "channel pump notifier lost owner".to_string())?;
                            match input_pump::run_channel_input_pump(current, Arc::clone(&adapter))
                                .await
                            {
                                Ok(()) | Err(input_pump::ChannelInputPumpError::ShuttingDown) => {
                                    break;
                                }
                                Err(input_pump::ChannelInputPumpError::DurableDebt(reason)) => {
                                    return Err(format!("channel input durable debt: {reason}"));
                                }
                                Err(error) => {
                                    owner = slot
                                        .resume_after_owner_loss()
                                        .map_err(|resume| resume.to_string())?;
                                    if owner.is_none() {
                                        return Err(error.to_string());
                                    }
                                }
                            }
                        }
                    }
                    Ok(())
                },
                &runtime,
            );
        Ok(())
    }

    pub(crate) fn begin_input_pump_shutdown(&self) -> Result<(), String> {
        self.input_pump_tasks
            .lock()
            .map_err(|_| "channel input pump task owner is unavailable".to_string())?
            .shutting_down = true;
        for slot in self
            .input_pumps
            .lock()
            .map_err(|_| "channel input pump registry is unavailable".to_string())?
            .values()
        {
            slot.begin_shutdown().map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    async fn observe_input_pump_settlement(&self) -> Result<ChannelPumpSettlement, String> {
        use futures::FutureExt;
        self.begin_input_pump_shutdown()?;
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| error.to_string())?;
        let settlement = {
            let mut tasks = self
                .input_pump_tasks
                .lock()
                .map_err(|_| "channel input pump task owner is unavailable".to_string())?;
            if let Some(receipt) = tasks
                .physical_receipt
                .lock()
                .map_err(|_| "Original pump physical receipt unavailable".to_string())?
                .clone()
            {
                return receipt;
            }
            if let Some(settlement) = tasks.settlement.as_ref() {
                settlement.clone()
            } else {
                let owned = tasks.tasks.clone();
                let failures = tasks.joined_failures.clone();
                // Waiters observe one producer. Original registry + joined
                // failures remain in this slot even if the producer panics.
                let producer = runtime.spawn(async move {
                    loop {
                        let result = futures::future::poll_fn(|context| match owned.lock() {
                            Ok(mut original) => original.poll_join_next(context).map(Ok),
                            Err(_) => std::task::Poll::Ready(Err(
                                "original pump JoinSet is unavailable".to_string(),
                            )),
                        })
                        .await?;
                        let Some(result) = result else {
                            break;
                        };
                        let failure = match result {
                            Ok(Ok(())) => None,
                            Ok(Err(error)) => Some(error),
                            Err(error) => Some(error.to_string()),
                        };
                        if let Some(error) = failure {
                            failures
                                .lock()
                                .map_err(|_| "original pump receipts unavailable".to_string())?
                                .push(error);
                        }
                    }
                    let failures = failures
                        .lock()
                        .map_err(|_| "original pump receipts unavailable".to_string())?
                        .clone();
                    Ok(ChannelPumpSettlement { failures })
                });
                let physical_receipt = tasks.physical_receipt.clone();
                let root = self
                    .lifecycle_retention
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                let observer = runtime.spawn(async move {
                    let receipt = producer
                        .await
                        .map_err(|error| {
                            format!("channel pump drain producer unavailable: {error}")
                        })
                        .and_then(|result| result);
                    *physical_receipt
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(receipt.clone());
                    if let Some(root) = root.upgrade() {
                        root.note_progress();
                    }
                    receipt
                });
                let settlement = async move {
                    observer.await.map_err(|error| {
                        format!("channel pump receipt observer unavailable: {error}")
                    })?
                }
                .boxed()
                .shared();
                tasks.settlement = Some(settlement.clone());
                settlement
            }
        };
        settlement.await
    }

    // Tests use a flattened error view. Production consumes the typed receipt
    // to distinguish completed joins with failures from unknown ownership.
    #[cfg(test)]
    pub(crate) async fn join_input_pumps(&self) -> Result<(), String> {
        let receipt = self.observe_input_pump_settlement().await?;
        if receipt.failures.is_empty() {
            Ok(())
        } else {
            Err(receipt.failures.join("; "))
        }
    }

    /// Reuse the same session/incarnation retirement authority after an explicit
    /// new transport start. Only known physical drain permits a fresh pump registry.
    fn reopen_input_pump_admission(&self) -> Result<(), String> {
        if !self
            .active_turns
            .lock()
            .map_err(|_| "Original channel foreground map unavailable".to_string())?
            .is_empty()
        {
            return Err("Original channel foreground ownership remains unsettled".to_string());
        }
        let mut tasks = self
            .input_pump_tasks
            .lock()
            .map_err(|_| "Original pump slot unavailable".to_string())?;
        if !tasks.shutting_down
            || !tasks
                .tasks
                .lock()
                .map_err(|_| "Original pump JoinSet unavailable".to_string())?
                .is_empty()
            || !tasks
                .physical_receipt
                .lock()
                .map_err(|_| "Original pump receipt unavailable".to_string())?
                .as_ref()
                .is_some_and(Result::is_ok)
        {
            return Err("Original pump drain is not physically confirmed".to_string());
        }
        self.input_pumps
            .lock()
            .map_err(|_| "Original pump registry unavailable".to_string())?
            .clear();
        self.session_wakes
            .lock()
            .map_err(|_| "Original session wake registry unavailable".to_string())?
            .clear();
        *tasks = ChannelInputPumpTasks::default();
        Ok(())
    }

    fn rotate(
        &self,
        surface_id: &ChannelSurfaceIdentity,
        instance: &echo_agent::channels::ChannelSessionInstance,
    ) -> Result<echo_agent::channels::ChannelSessionRotation, String> {
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = lifecycle.entry(surface_id.clone()).or_default();
        let current_incarnation_id = instance.incarnation_id();
        if record.current_incarnation_id.as_deref() != Some(current_incarnation_id.as_str()) {
            return Err(
                "channel session rotation rejected a stale coordinator authority".to_string(),
            );
        }
        let rotation = instance.rotate();
        if rotation.previous_incarnation_id() != current_incarnation_id {
            return Err("framework session incarnation changed during rotation".to_string());
        }
        record.current_incarnation_id = Some(rotation.incarnation_id().to_string());
        record.pending_ended_incarnation_id = None;
        Ok(rotation)
    }

    fn mark_incarnation_pending(gate: &ChannelSessionRetirementGate, incarnation_id: &str) {
        let mut obligations = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for obligation in obligations.values_mut() {
            if obligation.incarnation_id == incarnation_id
                && obligation.phase == ChannelRetirementPhase::Active
            {
                obligation.phase = ChannelRetirementPhase::RetirePending;
            }
        }
    }
}

#[cfg(feature = "channels")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChannelSessionRetirementError {
    Cancelled,
    Failed(String),
}

#[cfg(feature = "channels")]
impl std::fmt::Display for ChannelSessionRetirementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("channel session retirement cancelled"),
            Self::Failed(error) => write!(formatter, "channel session retirement failed: {error}"),
        }
    }
}

#[cfg(feature = "channels")]
impl std::error::Error for ChannelSessionRetirementError {}

#[cfg(feature = "channels")]
struct ChannelActiveTurnOwner {
    active_turns: ChannelActiveTurnMap,
    surface_id: ChannelSurfaceIdentity,
    turn_id: String,
}

#[cfg(feature = "channels")]
impl ChannelActiveTurnOwner {
    fn new(
        active_turns: ChannelActiveTurnMap,
        surface_id: ChannelSurfaceIdentity,
        turn_id: String,
    ) -> Self {
        Self {
            active_turns,
            surface_id,
            turn_id,
        }
    }
}

#[cfg(feature = "channels")]
impl Drop for ChannelActiveTurnOwner {
    fn drop(&mut self) {
        AppChannelMessageHandler::clear_active_turn(
            &self.active_turns,
            &self.surface_id,
            &self.turn_id,
        );
    }
}

#[cfg(feature = "channels")]
fn channel_cancel_barrier_complete(
    foreground_turns: &ForegroundTurnControl,
    workspace_id: &str,
    conversation_id: &str,
    root_turn_id: &str,
    result: &Result<crate::foreground_turn::ForegroundTurnSettlement, ForegroundTurnError>,
) -> bool {
    match result {
        Ok(_) => true,
        Err(ForegroundTurnError::NoActiveTurn { .. }) => foreground_turns
            .snapshots_for_workspace(workspace_id)
            .is_ok_and(|snapshots| {
                !snapshots.iter().any(|snapshot| {
                    snapshot.surface == ForegroundTurnSurface::Channel
                        && snapshot.conversation_id == conversation_id
                        && channel_snapshot_matches_root(snapshot, root_turn_id)
                })
            }),
        Err(_) => false,
    }
}

#[cfg(feature = "channels")]
async fn channel_cancel_root(
    foreground_turns: &ForegroundTurnControl,
    workspace_id: &str,
    conversation_id: &str,
    root_turn_id: &str,
) -> Result<crate::foreground_turn::ForegroundTurnSettlement, ForegroundTurnError> {
    foreground_turns
        .request_root_cancel_scoped(
            workspace_id,
            ForegroundTurnSurface::Channel,
            conversation_id,
            root_turn_id,
        )?
        .wait()
        .await
}

#[cfg(feature = "channels")]
fn channel_snapshot_matches_root(
    snapshot: &crate::foreground_turn::ForegroundTurnSnapshot,
    root_turn_id: &str,
) -> bool {
    snapshot.root_turn_id == root_turn_id
}

#[cfg(feature = "channels")]
fn channel_snapshot_for_conversation(
    snapshots: Vec<crate::foreground_turn::ForegroundTurnSnapshot>,
    conversation_id: &str,
) -> Result<Option<crate::foreground_turn::ForegroundTurnSnapshot>, String> {
    let mut matches = snapshots.into_iter().filter(|snapshot| {
        snapshot.surface == ForegroundTurnSurface::Channel
            && snapshot.conversation_id == conversation_id
    });
    let Some(snapshot) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(format!(
            "multiple channel foreground roots exist for conversation {conversation_id}"
        ));
    }
    Ok(Some(snapshot))
}

#[cfg(feature = "channels")]
fn channel_steer_target(snapshot: &crate::foreground_turn::ForegroundTurnSnapshot) -> &str {
    &snapshot.active_turn_id
}

#[cfg(feature = "channels")]
fn channel_active_generation_matches(active_turn_id: Option<&str>, owner_turn_id: &str) -> bool {
    active_turn_id.is_some_and(|active_turn_id| active_turn_id == owner_turn_id)
}

#[cfg(feature = "channels")]
async fn await_channel_retirement<T, RetireFuture>(
    cancel: echo_agent::agent::CancellationToken,
    retire: RetireFuture,
) -> Result<T, ChannelSessionRetirementError>
where
    RetireFuture: std::future::Future<Output = Result<T, String>>,
{
    tokio::select! {
        _ = cancel.cancelled() => Err(ChannelSessionRetirementError::Cancelled),
        result = retire => result.map_err(ChannelSessionRetirementError::Failed),
    }
}

#[cfg(feature = "channels")]
async fn await_channel_operation<Output, OperationFuture>(
    cancel: echo_agent::agent::CancellationToken,
    operation: OperationFuture,
) -> Option<Output>
where
    OperationFuture: std::future::Future<Output = Output>,
{
    tokio::select! {
        biased;
        result = operation => Some(result),
        _ = cancel.cancelled() => None,
    }
}

#[cfg(feature = "channels")]
fn channel_retirement_terminal(
    error: ChannelSessionRetirementError,
) -> (crate::chat_driver::TurnOutcome, String) {
    match error {
        ChannelSessionRetirementError::Cancelled => (
            crate::chat_driver::TurnOutcome::Cancelled,
            "Channel session retirement was cancelled; retry the message.".to_string(),
        ),
        ChannelSessionRetirementError::Failed(error) => (
            crate::chat_driver::TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                "channel_session_generation",
                error.clone(),
            )),
            format!("Unable to retire the previous channel session: {error}"),
        ),
    }
}

#[cfg(feature = "channels")]
fn is_task_run_control_command(command: &str) -> bool {
    matches!(
        command,
        "/subagent-message"
            | "/subagent-followup"
            | "/subagent-interrupt"
            | "/task-goal"
            | "/task-requirements"
            | "/task-requirement-skip"
            | "/task-run"
            | "/task-status"
            | "/task-pause"
            | "/task-resume"
            | "/task-cancel"
            | "/task-budget"
    )
}

#[cfg(feature = "channels")]
fn parse_channel_budget(value: &str, label: &str) -> Result<Option<u64>, String> {
    if matches!(value, "none" | "unbounded") {
        return Ok(None);
    }
    let budget = value
        .parse::<u64>()
        .map_err(|error| format!("Invalid {label} budget: {error}"))?;
    if budget == 0 {
        return Err(format!("{label} budget must be positive or 'none'."));
    }
    Ok(Some(budget))
}

#[cfg(feature = "channels")]
struct ChannelStreamDropGuard(echo_agent::agent::CancellationToken);

#[cfg(feature = "channels")]
impl Drop for ChannelStreamDropGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(feature = "channels")]
fn channel_render_event_stream(
    mut driver_rx: tokio::sync::mpsc::Receiver<ChannelRenderEvent>,
    mut prompt_rx: tokio::sync::broadcast::Receiver<String>,
    mut terminal_rx: tokio::sync::oneshot::Receiver<
        Result<crate::chat_driver::TurnOutcome, crate::chat_driver::ChatTurnError>,
    >,
    stream_drop_guard: ChannelStreamDropGuard,
) -> futures::stream::BoxStream<'static, echo_agent::error::Result<ChannelRenderEvent>> {
    use futures::StreamExt;

    async_stream::stream! {
        let _stream_drop_guard = stream_drop_guard;
        let mut driver_open = true;
        let mut prompt_open = true;
        let mut terminal_open = true;
        let mut terminal_outcome = None;
        loop {
            tokio::select! {
                event = driver_rx.recv(), if driver_open => match event {
                    Some(event) => yield Ok(event),
                    None => {
                        #[cfg(test)]
                        trace_channel_boundary("renderer.driver-eof");
                        driver_open = false;
                    },
                },
                prompt = prompt_rx.recv(), if prompt_open => match prompt {
                    Ok(prompt) => yield Ok(ChannelRenderEvent::Prompt(prompt)),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "channel HITL prompt receiver lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        prompt_open = false;
                    }
                },
                terminal = &mut terminal_rx, if terminal_open => {
                    terminal_open = false;
                    let terminal_error = match terminal {
                        Ok(Ok(outcome)) => {
                            #[cfg(test)]
                            if std::env::var_os("EKO_WEIXIN_A2_PHASE").is_some() {
                                eprintln!("A2_OWNER_BOUNDARY:renderer.physical-terminal senders={} queued={}", driver_rx.sender_strong_count(), driver_rx.len());
                            }
                            terminal_outcome = Some(outcome);
                            None
                        },
                        Ok(Err(error)) => Some(error.into_react_error()),
                        Err(_) => Some(echo_agent::error::ReactError::Other(
                            "channel foreground owner ended without a physical terminal receipt".to_string(),
                        )),
                    };
                    if let Some(error) = terminal_error {
                        // 原sink已接受的有限前缀先按序呈现；未结算观察不能等仍存活owner的EOF。
                        // 固定len避免活sender持续补入把错误传播变成无界drain，不新增队列或预算。
                        let accepted_events = driver_rx.len();
                        for _ in 0..accepted_events {
                            match driver_rx.try_recv() {
                                Ok(event) => yield Ok(event),
                                Err(_) => break,
                            }
                        }
                        yield Err(error);
                        break;
                    }
                }
            }
            // The single foreground owner spans every finite continuation turn.
            // Publish its terminal receipt only after the renderer channel has
            // closed, preserving all driver events and HITL prompts before Done.
            if !driver_open
                && let Some(outcome) = terminal_outcome.take()
            {
                #[cfg(test)]
                trace_channel_boundary("renderer.return-terminal");
                yield Ok(ChannelRenderEvent::Terminal(outcome));
                break;
            }
        }
    }
    .boxed()
}

#[cfg(feature = "channels")]
async fn channel_input_response_stream(
    receiver: input_pump::ChannelInputReplyReceiver,
    tool_executions: Arc<crate::tool_execution::ToolExecutionRepository>,
    service_cancel: Option<echo_agent::agent::CancellationToken>,
) -> futures::stream::BoxStream<
    'static,
    echo_agent::error::Result<echo_agent::channels::OutboundMessage>,
> {
    let correlation = receiver.correlation;
    let events = channel_render_event_stream(
        receiver.render_rx,
        tokio::sync::broadcast::channel(1).1,
        receiver.terminal_rx,
        ChannelStreamDropGuard(echo_agent::agent::CancellationToken::new()),
    );
    aggregate_by_sentence_with_repository(
        events,
        correlation.channel_id,
        correlation.to,
        correlation.chat_type,
        tool_executions,
        service_cancel,
    )
    .await
}

#[cfg(feature = "channels")]
async fn drive_deferred_weixin_reply(
    deferred: input_pump::DeferredWeixinReply,
    tool_executions: Arc<crate::tool_execution::ToolExecutionRepository>,
) {
    use echo_agent::channels::weixin::WeixinSendOutcome;
    use futures::StreamExt;
    #[cfg(test)]
    trace_channel_boundary("deferred-reply.enter");
    let input_pump::DeferredWeixinReply {
        scope,
        receiver,
        delivery,
        context,
        template,
        client_id_prefix,
    } = deferred;
    let mut stream = channel_input_response_stream(
        receiver,
        tool_executions,
        Some(scope.service_cancel.clone()),
    )
    .await;
    let mut ordinal = 0_u64;
    let mut sending = true;
    while let Some(item) = stream.next().await {
        let output = match item {
            Ok(output) => output,
            Err(_) => {
                sending = false;
                tracing::warn!(
                    "Weixin response rendering failed; original driver is still being observed"
                );
                continue;
            }
        };
        if !sending {
            continue;
        }
        if output.channel_id != template.channel_id
            || output.to != template.to
            || output.chat_type != template.chat_type
        {
            sending = false;
            tracing::warn!("Weixin original response target mismatch; output was not sent");
            continue;
        }
        let Some(next) = ordinal.checked_add(1) else {
            sending = false;
            tracing::warn!("Weixin response ordinal exhausted; output was not sent");
            continue;
        };
        ordinal = next;
        // Clone the ORIGINAL fence, never reconstruct it from an output or hold
        // a delivery permit while queued. Only the same producer admits send.
        let mut message = template.clone();
        message.text = output.text;
        message.attachments = output.attachments;
        if output
            .reply_to
            .as_ref()
            .is_some_and(|reply| template.reply_to.as_ref() != Some(reply))
        {
            sending = false;
            tracing::warn!("Weixin original provider reply identity mismatch; output was not sent");
            continue;
        }
        message.reply_to = template.reply_to.clone();
        let client_id = format!("{client_id_prefix}:{ordinal}");
        if scope.service_cancel.is_cancelled() {
            sending = false;
            continue;
        }
        let (_publication, _control) = match scope
            .state
            .channel_control_for_generation(
                &scope.address.workspace_id,
                &scope.binding.workspace_generation,
            )
            .await
        {
            Ok(guard) => guard,
            Err(_) => {
                sending = false;
                tracing::warn!("Weixin workspace generation changed before send");
                continue;
            }
        };
        if delivery
            .validate_message_context(
                &template.to,
                &scope.binding.provider_message_id,
                context.as_ref(),
            )
            .await
            .is_err()
        {
            sending = false;
            tracing::warn!("Original Weixin message association is no longer valid");
            continue;
        }
        if scope.service_cancel.is_cancelled() {
            sending = false;
            continue;
        }
        match delivery
            .send_with_context(message, context.as_ref(), &client_id)
            .await
        {
            Ok(WeixinSendOutcome::Accepted { .. }) => {}
            Ok(WeixinSendOutcome::Rejected { .. }) => {
                sending = false;
                tracing::warn!(
                    "Weixin rejected the original response; remaining output will be drained"
                );
            }
            Ok(WeixinSendOutcome::Unknown { .. }) | Err(_) => {
                sending = false;
                tracing::warn!(
                    "Weixin original response outcome is uncertain; no automatic resend"
                );
            }
        }
    }
    #[cfg(test)]
    trace_channel_boundary("deferred-reply.stream-eof");
}

/// IM channel 消息处理器：每 `handle` 从 AppState 的 exact scoped runtime 取/复用 per-sender agent。
///
/// IM channels drive chat through the shared foreground driver. Weixin pins
/// its original workspace generation at transport start; existing platforms
/// retain their original per-operation AppState resolution.
/// Whether a complex run is warranted is decided by the agent itself, not
/// pre-judged here.
#[cfg(feature = "channels")]
pub struct AppChannelMessageHandler {
    service_cancel: std::sync::Mutex<Option<tokio_util::sync::CancellationToken>>,
    weixin_identity: Option<(String, String)>,
    admission: Option<Arc<std::sync::atomic::AtomicBool>>,
    bound_control_runtime: Option<crate::state::ScopedChatRuntime>,
    bound_workspace_generation: Option<String>,
    weixin_delivery: Option<echo_agent::channels::weixin::WeixinDeliveryHandle>,
    app_state: Arc<crate::state::AppState>,
    webhook_emitter: Arc<crate::webhook::WebhookEmitter>,
    hitl: Arc<ChannelHumanLoopProvider>,
    human_loop_provider: Arc<dyn echo_agent::human_loop::HumanLoopProvider>,
    hitl_scope_resolver: crate::hitl::HitlScopeResolver,
    foreground_turns: ForegroundTurnControl,
    session_instance: echo_agent::channels::ChannelSessionInstance,
    session_coordinator: Arc<ChannelSessionCoordinator>,
    initialization_error: Option<String>,
    incarnation_fault: Arc<std::sync::Mutex<Option<String>>>,
    active_turns: ChannelActiveTurnMap,
    input_pump:
        Arc<input_pump::ChannelInputPumpSlot<crate::conversation_input::ConversationInputIdentity>>,
    pending_retirement: ChannelSessionRetirementGate,
}

#[cfg(feature = "channels")]
struct ChannelInputPumpItem {
    agent_conversation_id: String,
    cache_id: String,
    projection: crate::conversation_input::ConversationInputProjection,
    attempt: crate::conversation_input::ConversationInputAttempt,
    runtime: crate::state::ScopedChatRuntime,
    lease: std::sync::Mutex<Option<crate::foreground_turn::ForegroundTurnLease>>,
}

#[cfg(feature = "channels")]
struct ChannelRuntimeRetirementContext {
    app_state: Arc<crate::state::AppState>,
    foreground_turns: ForegroundTurnControl,
    active_turns: ChannelActiveTurnMap,
    pending_retirement: ChannelSessionRetirementGate,
}

#[cfg(feature = "channels")]
impl ChannelRuntimeRetirementContext {
    fn active_turn(&self, surface: &ChannelSurfaceIdentity) -> Option<ChannelActiveTurn> {
        self.active_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(surface)
            .cloned()
    }
    fn publish_active_turn(
        &self,
        surface: &ChannelSurfaceIdentity,
        conversation: &str,
        agent_conversation: &str,
        runtime: &crate::state::ScopedChatRuntime,
        turn: &str,
    ) -> Result<(), String> {
        publish_channel_active_turn(
            &self.active_turns,
            surface,
            conversation,
            agent_conversation,
            runtime,
            turn,
        )
    }
    async fn settle_pending_retirement_for_runtime(
        &self,
        runtime: &crate::state::ScopedChatRuntime,
        conversation_id: &str,
        agent_conversation_id: &str,
        surface_id: &ChannelSurfaceIdentity,
        cancel_predecessor: bool,
    ) -> Result<(), String> {
        let obligations = self.pending_runtime_obligations();
        if obligations.is_empty() {
            return Ok(());
        }
        if let Some(active) = self.active_turn(surface_id) {
            if !cancel_predecessor {
                return Err(
                    "an original control/retirement foreground still owns this channel scope"
                        .to_string(),
                );
            }
            let result = channel_cancel_root(
                &self.foreground_turns,
                active.runtime.execution_scope().workspace_id(),
                &active.conversation_id,
                &active.turn_id,
            )
            .await;
            let barrier_complete = channel_cancel_barrier_complete(
                &self.foreground_turns,
                active.runtime.execution_scope().workspace_id(),
                &active.conversation_id,
                &active.turn_id,
                &result,
            );
            if !barrier_complete {
                return Err(match result {
                    Ok(_) => "previous channel foreground did not reach its barrier".to_string(),
                    Err(error) => format!(
                        "unable to settle previous channel foreground before retirement: {error}"
                    ),
                });
            }
            AppChannelMessageHandler::clear_active_turn(
                &self.active_turns,
                surface_id,
                &active.turn_id,
            );
        }
        let retirement_turn_id = uuid::Uuid::new_v4().to_string();
        let retirement_lease = runtime
            .begin_turn(
                &self.foreground_turns,
                ForegroundTurnSurface::Channel,
                conversation_id,
                retirement_turn_id.clone(),
            )
            .await
            .map_err(|error| format!("Unable to admit channel session retirement: {error}"))?;
        if let Err(message) = self.publish_active_turn(
            surface_id,
            conversation_id,
            agent_conversation_id,
            runtime,
            &retirement_turn_id,
        ) {
            return match settle_channel_turn_after_input_observers(
                retirement_lease,
                crate::chat_driver::TurnOutcome::Cancelled,
            )
            .await
            {
                Ok(_) => Err(message),
                Err(error) => Err(format!("{message}; foreground settlement failed: {error}")),
            };
        }
        let retirement_owner = ChannelActiveTurnOwner::new(
            Arc::clone(&self.active_turns),
            surface_id.clone(),
            retirement_turn_id,
        );
        let retired = await_channel_retirement(retirement_lease.cancellation_token(), async {
            for obligation in obligations {
                let runtime = self.runtime_for_obligation(&obligation).await?;
                let pool = runtime
                    .pool()
                    .ok_or_else(|| "The recorded workspace has no AgentPool.".to_string())?;
                let retirement = pool
                    .begin_conversation_retirement(&obligation.key.runtime_state_id)
                    .map_err(|error| error.to_string())?;
                pool.drain_conversation_retirement(&retirement)
                    .await
                    .map_err(|error| error.to_string())?;
                self.set_runtime_obligation_phase(
                    &obligation.key,
                    ChannelRetirementPhase::GcPending,
                );
                AppChannelMessageHandler::clear_runtime_incarnation(
                    &runtime,
                    &obligation.product_conversation_id,
                    &obligation.key.runtime_state_id,
                )
                .await?;
                self.consume_runtime_obligation(&obligation.key);
                drop(retirement);
            }
            Ok(())
        })
        .await;
        match retired {
            Ok(()) => {
                settle_channel_turn_after_input_observers(
                    retirement_lease,
                    crate::chat_driver::TurnOutcome::Completed,
                )
                .await?;
                drop(retirement_owner);
                Ok(())
            }
            Err(error) => {
                let (outcome, message) = channel_retirement_terminal(error);
                let settlement =
                    settle_channel_turn_after_input_observers(retirement_lease, outcome).await;
                drop(retirement_owner);
                match settlement {
                    Ok(_) => Err(message),
                    Err(error) => Err(format!("{message}; foreground settlement failed: {error}")),
                }
            }
        }
    }

    fn record_runtime_owner(
        &self,
        runtime: &crate::state::ScopedChatRuntime,
        product_conversation_id: &str,
        runtime_state_id: &str,
        incarnation_id: &str,
    ) {
        let workspace = runtime.workspace_io_receipt();
        let key = ChannelRuntimeOwnerKey {
            workspace_id: workspace.workspace_id().to_string(),
            workspace_generation: workspace.host_generation().to_string(),
            runtime_state_id: runtime_state_id.to_string(),
        };
        let obligation = ChannelRuntimeObligation {
            key: key.clone(),
            product_conversation_id: product_conversation_id.to_string(),
            incarnation_id: incarnation_id.to_string(),
            phase: ChannelRetirementPhase::Active,
        };
        let mut obligations = self
            .pending_retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match obligations.get(&key) {
            Some(existing) if existing.phase != ChannelRetirementPhase::Active => {}
            _ => {
                obligations.insert(key, obligation);
            }
        }
    }

    fn pending_runtime_obligations(&self) -> Vec<ChannelRuntimeObligation> {
        self.pending_retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .filter(|obligation| obligation.phase != ChannelRetirementPhase::Active)
            .cloned()
            .collect()
    }

    fn set_runtime_obligation_phase(
        &self,
        key: &ChannelRuntimeOwnerKey,
        phase: ChannelRetirementPhase,
    ) {
        if let Some(obligation) = self
            .pending_retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(key)
        {
            obligation.phase = phase;
        }
    }

    fn consume_runtime_obligation(&self, key: &ChannelRuntimeOwnerKey) {
        self.pending_retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(key);
    }

    async fn runtime_for_obligation(
        &self,
        obligation: &ChannelRuntimeObligation,
    ) -> Result<crate::state::ScopedChatRuntime, String> {
        let runtime = self
            .app_state
            .chat_runtime_for_scope(&obligation.key.workspace_id)
            .await
            .map_err(|error| error.to_string())?;
        let receipt = runtime.workspace_io_receipt();
        if receipt.host_generation() != obligation.key.workspace_generation {
            return Err(format!(
                "workspace {} generation changed before channel runtime cleanup",
                obligation.key.workspace_id
            ));
        }
        Ok(runtime)
    }
}

#[cfg(feature = "channels")]
struct ChannelInputPumpAdapter {
    retirement: Option<Arc<ChannelRuntimeRetirementContext>>,
    weixin_identity: Option<(String, String)>,
    session_instance: Option<echo_agent::channels::ChannelSessionInstance>,
    reply_slot: Option<
        Arc<input_pump::ChannelInputPumpSlot<crate::conversation_input::ConversationInputIdentity>>,
    >,
    service_cancel: tokio_util::sync::CancellationToken,
    weixin_delivery: Option<echo_agent::channels::weixin::WeixinDeliveryHandle>,
    bound_control_runtime: Option<crate::state::ScopedChatRuntime>,
    active_turns: ChannelActiveTurnMap,
    surface_id: ChannelSurfaceIdentity,
    app_state: Arc<crate::state::AppState>,
    foreground_turns: ForegroundTurnControl,
    address: crate::conversation_input::ConversationInputAddress,
    agent_conversation_id: String,
    cache_id: String,
    hitl: Arc<ChannelHumanLoopProvider>,
    human_loop_provider: Arc<dyn echo_agent::human_loop::HumanLoopProvider>,
}

/// Everything the original handler needs to re-enter its own pump slot.
/// Cloning these handles does not create an owner, queue or settlement path.
#[cfg(feature = "channels")]
#[derive(Clone)]
struct ChannelPumpWiring {
    retirement: Option<Arc<ChannelRuntimeRetirementContext>>,
    weixin_identity: Option<(String, String)>,
    session_instance: Option<echo_agent::channels::ChannelSessionInstance>,
    reply_slot: Option<
        Arc<input_pump::ChannelInputPumpSlot<crate::conversation_input::ConversationInputIdentity>>,
    >,
    service_cancel: tokio_util::sync::CancellationToken,
    weixin_delivery: Option<echo_agent::channels::weixin::WeixinDeliveryHandle>,
    bound_control_runtime: Option<crate::state::ScopedChatRuntime>,
    active_turns: ChannelActiveTurnMap,
    surface_id: ChannelSurfaceIdentity,
    // Weak: the coordinator retains this wake, so a strong handle would make
    // the registry's own type recursive.
    coordinator: std::sync::Weak<ChannelSessionCoordinator>,
    // Weak for the same reason: AppState reaches this registry through the
    // channel runtime, so a strong handle would recurse its Sync evaluation.
    app_state: std::sync::Weak<crate::state::AppState>,
    foreground_turns: ForegroundTurnControl,
    hitl: Arc<ChannelHumanLoopProvider>,
    human_loop_provider: Arc<dyn echo_agent::human_loop::HumanLoopProvider>,
}

#[cfg(feature = "channels")]
impl ChannelPumpWiring {
    fn adapter(
        &self,
        address: crate::conversation_input::ConversationInputAddress,
        agent_conversation_id: String,
        cache_id: String,
    ) -> Result<ChannelInputPumpAdapter, String> {
        let app_state = self
            .app_state
            .upgrade()
            .ok_or_else(|| "original channel app state unavailable".to_string())?;
        Ok(ChannelInputPumpAdapter {
            retirement: self.retirement.clone(),
            weixin_identity: self.weixin_identity.clone(),
            session_instance: self.session_instance.clone(),
            reply_slot: self.reply_slot.clone(),
            service_cancel: self.service_cancel.clone(),
            weixin_delivery: self.weixin_delivery.clone(),
            bound_control_runtime: self.bound_control_runtime.clone(),
            active_turns: Arc::clone(&self.active_turns),
            surface_id: self.surface_id.clone(),
            app_state,
            foreground_turns: self.foreground_turns.clone(),
            address,
            agent_conversation_id,
            cache_id,
            hitl: Arc::clone(&self.hitl),
            human_loop_provider: Arc::clone(&self.human_loop_provider),
        })
    }
}

/// The original durable handler's retained wake capability. It lets a user
/// action re-enter the same slot after the queue stops being blocked; it does
/// not own scheduling, retry or recovery state.
#[cfg(feature = "channels")]
struct ChannelSessionWake {
    wiring: ChannelPumpWiring,
}

#[cfg(feature = "channels")]
impl ChannelSessionWake {
    /// Wake the original pump after an operator abandoned one pending input.
    /// Deriving the address from the retained instance keeps this action on
    /// the original conversation instead of the current GUI selection.
    async fn start_after_abandon(&self) -> Result<&'static str, String> {
        let instance = self
            .wiring
            .session_instance
            .as_ref()
            .ok_or_else(|| "original channel session instance unavailable".to_string())?;
        let slot = self
            .wiring
            .reply_slot
            .clone()
            .ok_or_else(|| "original channel pump slot unavailable".to_string())?;
        let workspace_id = self
            .wiring
            .bound_control_runtime
            .as_ref()
            .map(|runtime| runtime.execution_scope().workspace_id().to_string())
            .ok_or_else(|| "original channel workspace is unavailable".to_string())?;
        let conversation = AppChannelMessageHandler::conversation_id(
            instance.channel_id(),
            instance.conversation_id(),
            instance.sender_id(),
        );
        let agent_conversation = AppChannelMessageHandler::agent_conversation_id(
            &conversation,
            &instance.incarnation_id(),
        );
        let cache_id =
            AppChannelMessageHandler::cache_user_id(&conversation, &instance.incarnation_id());
        let coordinator = self
            .wiring
            .coordinator
            .upgrade()
            .ok_or_else(|| "original channel session coordinator unavailable".to_string())?;
        start_channel_input_pump_with(
            &coordinator,
            slot,
            &self.wiring,
            crate::conversation_input::ConversationInputAddress {
                workspace_id,
                conversation_id: conversation,
            },
            agent_conversation,
            cache_id,
        )
        .await
    }
}

#[cfg(feature = "channels")]
async fn start_channel_input_pump_with(
    coordinator: &Arc<ChannelSessionCoordinator>,
    slot: Arc<
        input_pump::ChannelInputPumpSlot<crate::conversation_input::ConversationInputIdentity>,
    >,
    wiring: &ChannelPumpWiring,
    address: crate::conversation_input::ConversationInputAddress,
    agent_conversation_id: String,
    cache_id: String,
) -> Result<&'static str, String> {
    match slot.kick().map_err(|error| error.to_string())? {
        input_pump::ChannelInputPumpKick::Notified => Ok("notified"),
        input_pump::ChannelInputPumpKick::Started(owner) => {
            coordinator.start_input_pump_task(
                slot,
                owner,
                Arc::new(wiring.adapter(address, agent_conversation_id, cache_id)?),
            )?;
            Ok("started")
        }
    }
}

#[cfg(feature = "channels")]
fn hand_weixin_steer_to_original_owner<T>(
    sender: tokio::sync::oneshot::Sender<T>,
    result: T,
) -> Result<ChannelLiveInputRoute, String> {
    sender
        .send(result)
        .map_err(|_| "The original steer supervisor no longer owns its receiver".to_string())?;
    Ok(ChannelLiveInputRoute::WeixinAcceptedPending)
}

#[cfg(feature = "channels")]
enum ChannelLiveInputRoute {
    Routed,
    WeixinAcceptedPending,
    PumpPending(Box<input_pump::ChannelInputReplyRoute>),
}

#[cfg(feature = "channels")]
impl input_pump::ChannelInputPumpAdapter for ChannelInputPumpAdapter {
    type Identity = crate::conversation_input::ConversationInputIdentity;
    type Item = ChannelInputPumpItem;

    fn peek_next_identity(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<Option<Self::Identity>, String>> {
        Box::pin(async move {
            if self.service_cancel.is_cancelled() {
                return Ok(None);
            }
            if self.surface_id.channel_id == "weixin" {
                let (bindings, _inventory_complete) = self
                    .app_state
                    .storage
                    .chat_events
                    .channel_input_bindings()
                    .await
                    .map_err(|error| error.to_string())?;
                let mut addresses = Vec::new();
                for input in bindings {
                    let Some(binding) = input.payload.channel_binding.as_ref() else {
                        continue;
                    };
                    let target = echo_agent::channels::weixin::encode_target(
                        &binding.account_id,
                        &binding.peer_id,
                    )
                    .map_err(|error| error.to_string())?;
                    if target == self.surface_id.sender_id
                        && !addresses.contains(&input.receipt.identity.address)
                    {
                        addresses.push(input.receipt.identity.address);
                    }
                }
                for address in addresses {
                    let frontier = self
                        .app_state
                        .conversation_inputs()
                        .list(&address)
                        .await
                        .map_err(|error| error.to_string())?;
                    if frontier
                        .items
                        .iter()
                        .any(|input| input.receipt.blocks_replay())
                    {
                        // Unknown ownership blocks its original address, not
                        // independently proven DATA in a different workspace.
                        continue;
                    }
                    if let Some(input) = frontier.items.first() {
                        return Ok(Some(input.receipt.identity.clone()));
                    }
                }
                return Ok(None);
            }
            self.app_state
                .conversation_inputs()
                .list(&self.address)
                .await
                .map(|frontier| {
                    frontier
                        .items
                        .first()
                        .map(|item| item.receipt.identity.clone())
                })
                .map_err(|error| error.to_string())
        })
    }

    fn recover_unroutable<'a>(
        &'a self,
        identity: &'a Self::Identity,
    ) -> futures::future::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if self.surface_id.channel_id == "weixin" {
                use crate::conversation_input::FrozenChannelContext;
                let restored = async {
                    if self.service_cancel.is_cancelled() {
                        return Err("channel service closed during route recovery".to_string());
                    }
                    let input = self
                        .app_state
                        .storage
                        .chat_events
                        .prove_unclaimed_channel_input(identity.clone())
                        .await
                        .map_err(|error| error.to_string())?;
                    let binding = input
                        .payload
                        .channel_binding
                        .as_ref()
                        .ok_or_else(|| "original channel DATA unavailable".to_string())?;
                    if !self
                        .weixin_identity
                        .as_ref()
                        .is_some_and(|(account, generation)| {
                            account == &binding.account_id
                                && generation == &binding.credential_generation
                        })
                    {
                        return Err(
                            "saved Weixin credential generation is no longer active".to_string()
                        );
                    }
                    let (_publication, _control) = self
                        .app_state
                        .channel_control_for_generation(
                            &identity.address.workspace_id,
                            &binding.workspace_generation,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    let delivery = self
                        .weixin_delivery
                        .clone()
                        .ok_or_else(|| "original Weixin delivery unavailable".to_string())?;
                    let target = echo_agent::channels::weixin::encode_target(
                        &binding.account_id,
                        &binding.peer_id,
                    )
                    .map_err(|error| error.to_string())?;
                    let context = match &binding.context {
                        FrozenChannelContext::Absent => None,
                        FrozenChannelContext::Reference(reference) => Some(
                            echo_agent::channels::weixin::WeixinContextRef::from(reference.clone()),
                        ),
                    };
                    delivery
                        .validate_message_context(
                            &target,
                            &binding.provider_message_id,
                            context.as_ref(),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    let instance = self
                        .session_instance
                        .as_ref()
                        .ok_or_else(|| "original channel incarnation unavailable".to_string())?;
                    if instance.channel_id() != "weixin"
                        || instance.conversation_id() != target
                        || instance.sender_id() != target
                    {
                        return Err("saved channel peer does not match the original incarnation"
                            .to_string());
                    }
                    let message = echo_agent::channels::InboundMessage::new(
                        "weixin",
                        target.clone(),
                        &target,
                        echo_agent::channels::ChatType::Direct,
                        input.payload.text,
                        binding.provider_message_id.clone(),
                    );
                    let template = instance.bind_outbound(
                        echo_agent::channels::OutboundMessage::new(
                            "weixin",
                            &target,
                            echo_agent::channels::ChatType::Direct,
                            String::new(),
                        )
                        .with_reply_to(binding.provider_message_id.clone()),
                    );
                    let (mut route, receiver) = input_pump::channel_input_reply_route(&message);
                    route.deferred_weixin = Some(input_pump::DeferredWeixinReply {
                        scope: input_pump::WeixinReplyScope {
                            recovered: true,
                            state: self.app_state.clone(),
                            address: identity.address.clone(),
                            binding: binding.clone(),
                            service_cancel: self.service_cancel.clone(),
                        },
                        receiver,
                        delivery,
                        context,
                        template,
                        client_id_prefix: format!("eko-input:{}", identity.input_id),
                    });
                    self.reply_slot
                        .as_ref()
                        .ok_or_else(|| "original route slot unavailable".to_string())?
                        .restore_reply(identity.clone(), route)
                        .map_err(|error| error.to_string())
                }
                .await;
                if let Err(error) = &restored {
                    if self.service_cancel.is_cancelled() {
                        return Ok(());
                    }
                    self.app_state
                        .storage
                        .chat_events
                        .mark_channel_input_uncertain(identity.clone(), error.clone())
                        .await
                        .map_err(|error| error.to_string())?;
                    // The original durable RecoveryRequired fact resolved
                    // route recovery. Other independently safe addresses may
                    // proceed; no failure is converted into a successful run.
                    return Ok(());
                }
                return restored;
            }
            match self
                .app_state
                .conversation_inputs()
                .cancel(identity.clone())
                .await
            {
                Ok(_) => Ok(()),
                Err(error) => {
                    let frontier = self
                        .app_state
                        .conversation_inputs()
                        .list(&self.address)
                        .await
                        .map_err(|list_error| list_error.to_string())?;
                    if frontier
                        .items
                        .first()
                        .is_none_or(|item| item.receipt.identity != *identity)
                    {
                        Ok(())
                    } else {
                        Err(error.to_string())
                    }
                }
            }
        })
    }

    fn claim_next<'a>(
        &'a self,
        expected_identity: &'a Self::Identity,
    ) -> futures::future::BoxFuture<'a, Result<Option<Self::Item>, String>> {
        Box::pin(async move {
            let frontier = self
                .app_state
                .conversation_inputs()
                .list(&expected_identity.address)
                .await
                .map_err(|error| error.to_string())?;
            let Some(next) = frontier.items.first() else {
                return Ok(None);
            };
            if next.receipt.identity != *expected_identity {
                return Ok(None);
            }
            if self.service_cancel.is_cancelled() {
                return Ok(None);
            }
            let mut publication = None;
            let runtime = if let Some(binding) = next.payload.channel_binding.as_ref() {
                if !self
                    .weixin_identity
                    .as_ref()
                    .is_some_and(|(account, generation)| {
                        account == &binding.account_id
                            && generation == &binding.credential_generation
                    })
                {
                    return Err(
                        "saved Weixin credential generation is no longer active".to_string()
                    );
                }
                let (guard, control) = self
                    .app_state
                    .channel_control_for_generation(
                        &expected_identity.address.workspace_id,
                        &binding.workspace_generation,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                publication = Some(guard);
                let delivery = self
                    .weixin_delivery
                    .as_ref()
                    .ok_or_else(|| "original Weixin sender unavailable".to_string())?;
                let context = match &binding.context {
                    crate::conversation_input::FrozenChannelContext::Absent => None,
                    crate::conversation_input::FrozenChannelContext::Reference(reference) => Some(
                        echo_agent::channels::weixin::WeixinContextRef::from(reference.clone()),
                    ),
                };
                let target = echo_agent::channels::weixin::encode_target(
                    &binding.account_id,
                    &binding.peer_id,
                )
                .map_err(|error| error.to_string())?;
                delivery
                    .validate_message_context(
                        &target,
                        &binding.provider_message_id,
                        context.as_ref(),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                control.runtime().clone()
            } else {
                match &self.bound_control_runtime {
                    Some(runtime) => runtime.clone(),
                    None => self
                        .app_state
                        .chat_runtime_for_scope(&expected_identity.address.workspace_id)
                        .await
                        .map_err(|error| error.to_string())?,
                }
            };
            if runtime.execution_scope().workspace_id() != expected_identity.address.workspace_id {
                return Err("channel input workspace changed before claim".to_string());
            }
            // Use the SAME original retirement gate before acquiring this
            // input's foreground lease; never wait for it in short admission.
            drop(publication.take());
            // One original pump may await actual foreground capacity. Short
            // ingress calls never park here and no input-specific task exists.
            for active in self
                .foreground_turns
                .snapshots_for_workspace(&expected_identity.address.workspace_id)
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|active| {
                    active.conversation_id == expected_identity.address.conversation_id
                })
            {
                let waiter = match self.foreground_turns.settlement_waiter_scoped(
                    &active.workspace_id,
                    active.surface,
                    &active.conversation_id,
                    &active.root_turn_id,
                ) {
                    Ok(waiter) => waiter,
                    Err(
                        ForegroundTurnError::NoActiveTurn { .. }
                        | ForegroundTurnError::TurnMismatch { .. },
                    ) => continue,
                    Err(error) => return Err(error.to_string()),
                };
                tokio::select! {
                    biased;
                    _ = self.service_cancel.cancelled() => return Ok(None),
                    result = waiter.wait() => { result.map_err(|error| error.to_string())?; }
                }
            }
            let incarnation = self
                .session_instance
                .as_ref()
                .map(|instance| instance.incarnation_id());
            let (agent_conversation_id, cache_id) = match incarnation.as_deref() {
                Some(incarnation) => (
                    AppChannelMessageHandler::agent_conversation_id(
                        &expected_identity.address.conversation_id,
                        incarnation,
                    ),
                    AppChannelMessageHandler::cache_user_id(
                        &expected_identity.address.conversation_id,
                        incarnation,
                    ),
                ),
                None => (self.agent_conversation_id.clone(), self.cache_id.clone()),
            };
            if let Some(retirement) = self.retirement.as_ref() {
                retirement
                    .settle_pending_retirement_for_runtime(
                        &runtime,
                        &expected_identity.address.conversation_id,
                        &agent_conversation_id,
                        &self.surface_id,
                        false,
                    )
                    .await?;
                retirement.record_runtime_owner(
                    &runtime,
                    &expected_identity.address.conversation_id,
                    &agent_conversation_id,
                    incarnation.as_deref().unwrap_or_default(),
                );
            }
            if let Some(binding) = next.payload.channel_binding.as_ref() {
                let (guard, _) = self
                    .app_state
                    .channel_control_for_generation(
                        &expected_identity.address.workspace_id,
                        &binding.workspace_generation,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                publication = Some(guard);
            }
            let recovered_channel = if next.payload.channel_binding.is_some() {
                match self.reply_slot.as_ref() {
                    Some(slot) => slot
                        .requires_channel_recovery_proof(expected_identity)
                        .map_err(|error| error.to_string())?,
                    None => true,
                }
            } else {
                false
            };
            let turn_id = format!("channel-input-turn:{}", expected_identity.input_id);
            let lease = match runtime
                .begin_turn(
                    &self.foreground_turns,
                    ForegroundTurnSurface::Channel,
                    &expected_identity.address.conversation_id,
                    turn_id.clone(),
                )
                .await
            {
                Ok(lease) => lease,
                Err(crate::conversation_deletion::ConversationDeletionError::Foreground(
                    ForegroundTurnError::Busy { .. },
                )) => {
                    return Err("channel foreground is busy; pump will retry".to_string());
                }
                Err(error) => return Err(error.to_string()),
            };
            if let Err(error) = publish_channel_active_turn(
                &self.active_turns,
                &self.surface_id,
                &expected_identity.address.conversation_id,
                &agent_conversation_id,
                &runtime,
                &turn_id,
            ) {
                settle_channel_turn_after_input_observers(
                    lease,
                    crate::chat_driver::TurnOutcome::Cancelled,
                )
                .await
                .map_err(|settle| format!("{error}; foreground settlement failed: {settle}"))?;
                return Err(error);
            }
            lease.retain(ChannelActiveTurnOwner::new(
                self.active_turns.clone(),
                self.surface_id.clone(),
                turn_id.clone(),
            ));
            let projection = match self
                .app_state
                .storage
                .chat_events
                .start_selected_conversation_input_checked(
                    expected_identity.clone(),
                    frontier.queue_revision,
                    turn_id,
                    recovered_channel,
                    Some(self.service_cancel.clone()),
                )
                .await
            {
                Ok(projection) => projection,
                Err(error) => {
                    settle_channel_turn_after_input_observers(
                        lease,
                        crate::chat_driver::TurnOutcome::Cancelled,
                    )
                    .await
                    .map_err(|settle| format!("{error}; foreground settlement failed: {settle}"))?;
                    return Err(error.to_string());
                }
            };
            let attempt = match channel_input_attempt(&projection) {
                Ok(attempt) => attempt,
                Err(error) => {
                    let failure = crate::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "conversation_input",
                            error.clone(),
                        ),
                    );
                    if let Err(settle_error) = lease.settle_after_observers(failure).await {
                        return Err(format!(
                            "{error}; malformed claim settlement failed: {settle_error}"
                        ));
                    }
                    return Err(error);
                }
            };
            drop(publication);
            Ok(Some(ChannelInputPumpItem {
                agent_conversation_id,
                cache_id,
                projection,
                attempt,
                runtime,
                lease: std::sync::Mutex::new(Some(lease)),
            }))
        })
    }

    fn identity<'a>(&self, item: &'a Self::Item) -> &'a Self::Identity {
        &item.attempt.identity
    }

    fn execute_claimed<'a>(
        &'a self,
        item: &'a Self::Item,
        reply: input_pump::ChannelInputReplyRoute,
    ) -> futures::future::BoxFuture<'a, Result<(), input_pump::ChannelInputExecutionError>> {
        Box::pin(async move {
            let cancel = match item.lease.lock() {
                Ok(lease) => match lease.as_ref() {
                    Some(lease) => lease.cancellation_token(),
                    None => {
                        return Err(input_pump::ChannelInputExecutionError::before_driver(
                            "channel input lease was already consumed",
                            reply,
                        ));
                    }
                },
                Err(_) => {
                    return Err(input_pump::ChannelInputExecutionError::before_driver(
                        "channel input lease is unavailable",
                        reply,
                    ));
                }
            };
            let _effect_publication =
                if let Some(binding) = item.projection.payload.channel_binding.as_ref() {
                    match self
                        .app_state
                        .channel_control_for_generation(
                            &item.attempt.identity.address.workspace_id,
                            &binding.workspace_generation,
                        )
                        .await
                    {
                        Ok((publication, _)) => Some(publication),
                        Err(error) => {
                            return Err(input_pump::ChannelInputExecutionError::before_driver(
                                error.to_string(),
                                reply,
                            ));
                        }
                    }
                } else {
                    None
                };
            if self.service_cancel.is_cancelled() {
                return Err(input_pump::ChannelInputExecutionError::before_driver(
                    "channel service closed before effect",
                    reply,
                ));
            }
            let intent = item
                .projection
                .payload
                .channel_binding
                .as_ref()
                .map(|binding| binding.execution.clone())
                .unwrap_or(crate::conversation_input::ChannelExecutionIntent::Chat);
            let resume_control = if matches!(
                &intent,
                crate::conversation_input::ChannelExecutionIntent::Resume { .. }
            ) {
                let root = format!("weixin-resume:{}", item.attempt.identity.input_id);
                let admission = self.app_state.storage.chat_events.claim_weixin_control(
                    &item.attempt.identity.address.workspace_id,
                    &item.attempt.identity.address.conversation_id,
                    &root,
                    &item.projection.payload.payload_sha256,
                );
                match admission {
                    Ok(crate::chat_event_log::WeixinControlAdmission::Execute(lease)) => {
                        let (observer, receiver) = ControlEffectObserver::new(*lease);
                        drop(receiver);
                        Some(observer)
                    }
                    Ok(_) => {
                        return Err(input_pump::ChannelInputExecutionError::before_driver(
                            "original resume control cannot be claimed again",
                            reply,
                        ));
                    }
                    Err(error) => {
                        return Err(input_pump::ChannelInputExecutionError::before_driver(
                            error.to_string(),
                            reply,
                        ));
                    }
                }
            } else {
                None
            };
            let Some(pool) = item.runtime.pool() else {
                return Err(input_pump::ChannelInputExecutionError::before_driver(
                    "channel input runtime has no AgentPool",
                    reply,
                ));
            };
            // 仅实际经过 conversation driver 的入口延后准备；Planned Resume
            // 由计划执行器接管，保留完整初始化，不给持久会话留下未确认标记。
            let prepare_in_driver = match &intent {
                crate::conversation_input::ChannelExecutionIntent::Chat => true,
                crate::conversation_input::ChannelExecutionIntent::Resume {
                    expected,
                    driver_continuation_enabled,
                } => matches!(
                    channel_resume_dispatch(
                        expected.clone(),
                        *driver_continuation_enabled,
                        &item.attempt.turn_id
                    ),
                    ChannelResumeDispatch::Continuation(_)
                ),
            };
            let admission = if prepare_in_driver {
                pool.acquire_for_turn(&item.agent_conversation_id).await
            } else {
                pool.acquire(&item.agent_conversation_id).await
            };
            let execution = match admission {
                Ok(execution) => execution,
                Err(error) => {
                    return Err(input_pump::ChannelInputExecutionError::before_driver(
                        error.to_string(),
                        reply,
                    ));
                }
            };
            let agent = execution.agent();
            configure_channel_agent(&agent, &item.cache_id, self.human_loop_provider.clone()).await;
            let flow = match self
                .app_state
                .session
                .product_data_io
                .begin_owned_flow("prepare pumped channel input")
            {
                Ok(flow) => flow,
                Err(error) => {
                    return Err(input_pump::ChannelInputExecutionError::before_driver(
                        error.to_string(),
                        reply,
                    ));
                }
            };
            let turn = match prepare_channel_turn(
                ChannelTurnPreparation {
                    attachments: item.projection.payload.attachments.clone(),
                    execution_root: item.runtime.execution_scope().root().to_path_buf(),
                    text: channel_input_effective_text(&item.projection.payload),
                    conversation_id: item.attempt.identity.address.conversation_id.clone(),
                    turn_id: item.attempt.turn_id.clone(),
                    runtime_authored: item
                        .projection
                        .payload
                        .channel_binding
                        .as_ref()
                        .is_some_and(|binding| {
                            matches!(
                                &binding.execution,
                                crate::conversation_input::ChannelExecutionIntent::Resume { .. }
                            )
                        }),
                    workspace_io_receipt: item.runtime.workspace_io_receipt(),
                },
                &flow,
            )
            .await
            {
                Ok(turn) => {
                    flow.settle(None);
                    turn
                }
                Err(error) => {
                    flow.settle(Some(error.clone()));
                    return Err(input_pump::ChannelInputExecutionError::before_driver(
                        error, reply,
                    ));
                }
            };
            let lease = match item.lease.lock() {
                Ok(mut lease) => match lease.take() {
                    Some(lease) => lease,
                    None => {
                        return Err(input_pump::ChannelInputExecutionError::before_driver(
                            "channel input lease was already consumed",
                            reply,
                        ));
                    }
                },
                Err(_) => {
                    return Err(input_pump::ChannelInputExecutionError::before_driver(
                        "channel input lease is unavailable",
                        reply,
                    ));
                }
            };
            let mut reply = reply;
            let deferred_weixin = reply.deferred_weixin.take();
            let lifecycle_render_tx = reply.render_tx.clone();
            let lifecycle_terminal_tx = reply.render_tx.clone();
            let lifecycle_cursor = Arc::clone(&reply.lifecycle_cursor);
            let terminal_lifecycle_cursor = Arc::clone(&reply.lifecycle_cursor);
            let lifecycle_log = Arc::clone(&self.app_state.storage.chat_events);
            let terminal_lifecycle_log = Arc::clone(&self.app_state.storage.chat_events);
            let lifecycle_identity = item.attempt.identity.clone();
            let terminal_lifecycle_identity = item.attempt.identity.clone();
            let prompt_tx = reply.render_tx.clone();
            let prompts = self.hitl.subscribe_prompts();
            let inner_sink: Arc<dyn crate::chat_driver::ChatSink> =
                Arc::new(ChannelSurfaceSink::new(reply.render_tx, cancel.clone()));
            let terminal_tx = reply.terminal_tx;
            let sink = crate::chat_event_log::bind_surface_chat_sink(
                crate::chat_event_log::ChatSurface::Channel,
                inner_sink,
                self.app_state.storage.chat_events.clone(),
                self.app_state.storage.tool_executions.clone(),
                item.attempt.identity.address.workspace_id.clone(),
                Some(item.attempt.identity.address.conversation_id.clone()),
                item.attempt.turn_id.clone(),
            );
            let driver_cancel = cancel.clone();
            let resources = Arc::new(crate::chat_resources::ChatResources {
                execution_scope: item.runtime.execution_scope().clone(),
                workspace_io_receipt: Some(item.runtime.workspace_io_receipt()),
                pool: Some(pool),
                store: item.runtime.task_runtime(),
                sink,
                webhook_emitter: None,
                conv_id: Some(item.attempt.identity.address.conversation_id.clone()),
                root_message_id: item.attempt.turn_id.clone(),
                attachments: turn.inline_attachment_refs(),
                cancel,
                review_integration: item.runtime.review_integration(),
                memory_generation: None,
                human_loop_provider: Some(self.human_loop_provider.clone()),
            });
            let observer_service = self.app_state.conversation_inputs();
            let observer_attempt = item.attempt.clone();
            let observed_resume_control = resume_control.clone();
            let observer: crate::chat_driver::InputReceiptObserver = Arc::new(move |receipt| {
                let service = observer_service.clone();
                let observed_control = observed_resume_control.clone();
                let attempt = observer_attempt.clone();
                let render_tx = lifecycle_render_tx.clone();
                let cursor = Arc::clone(&lifecycle_cursor);
                let log = Arc::clone(&lifecycle_log);
                let identity = lifecycle_identity.clone();
                Box::pin(async move {
                    // This callback is invoked by the original driver AFTER
                    // its exact RunTurn claim and Domain preparation, before
                    // rendering. It supplies admission truth, not Goal outcome.
                    if let Some(control) = observed_control {
                        control.report(ControlEffectOutcome::Applied).await;
                    }
                    let observed = service
                        .observe_turn_input_through_drain(attempt, receipt)
                        .await;
                    let projected = project_channel_committed_events(
                        log.as_ref(),
                        &identity,
                        &render_tx,
                        cursor.as_ref(),
                    )
                    .await;
                    match (observed, projected) {
                        (Ok(_), Ok(())) => Ok(()),
                        (Err(error), Ok(())) => Err(error.to_string()),
                        (Ok(_), Err(error)) => {
                            tracing::warn!(%error, "channel input lifecycle projection was not delivered");
                            Ok(())
                        }
                        (Err(observed), Err(projected)) => {
                            tracing::warn!(error = %projected, "channel input failure lifecycle projection was not delivered");
                            Err(observed.to_string())
                        }
                    }
                })
            });
            let terminal_service = self.app_state.conversation_inputs();
            let terminal_attempt = item.attempt.clone();
            let durable_terminal = move |outcome| {
                let service = terminal_service.clone();
                let attempt = terminal_attempt.clone();
                let render_tx = lifecycle_terminal_tx.clone();
                let cursor = Arc::clone(&terminal_lifecycle_cursor);
                let log = Arc::clone(&terminal_lifecycle_log);
                let identity = terminal_lifecycle_identity.clone();
                async move {
                    service
                        .settle_attempt(&attempt, &outcome)
                        .await
                        .map_err(|error| error.to_string())?;
                    if let Err(error) = project_channel_committed_events(
                        log.as_ref(),
                        &identity,
                        &render_tx,
                        cursor.as_ref(),
                    )
                    .await
                    {
                        tracing::warn!(%error, "channel input terminal lifecycle projection was not delivered");
                    }
                    Ok(())
                }
            };
            // 先将原执行 future 放到堆上，再交给提示、取消与回复的组合 future。
            // 内联副本会放大 execute_claimed 的 poll 栈帧，耗尽正常线程栈；
            // 下方仍由同一所有者轮询并结算该操作。
            let drive = Box::pin(async move {
                let target_run_id = match &intent {
                    crate::conversation_input::ChannelExecutionIntent::Chat => {
                        crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(
                            &item.attempt.turn_id,
                        )
                    }
                    crate::conversation_input::ChannelExecutionIntent::Resume {
                        expected, ..
                    } => expected.run_id.clone(),
                };
                let input_source = match self
                    .app_state
                    .bind_user_input_target(
                        crate::conversation_input::ConversationInputSource::Channel,
                        &item.projection,
                        &target_run_id,
                        &item.attempt.turn_id,
                    )
                    .await
                {
                    Ok(input) => input,
                    Err(error) => {
                        let service = self.app_state.conversation_inputs();
                        let admission_error = service
                            .recovery_required(item.attempt.clone(), error.clone())
                            .await
                            .err();
                        if let Some(control) = resume_control.as_ref() {
                            control
                                .report(ControlEffectOutcome::Unknown {
                                    reason: error.clone(),
                                })
                                .await;
                        }
                        drop(_effect_publication);
                        let detail = admission_error
                            .map_or(error.clone(), |failure| format!("{error}; {failure}"));
                        return lease
                            .settle_after(
                                crate::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message(
                                        "input_admission",
                                        detail,
                                    ),
                                ),
                                durable_terminal,
                            )
                            .await
                            .map(|settled| settled.outcome)
                            .map_err(|error| {
                                crate::chat_driver::ChatTurnError::from(error.to_string())
                            });
                    }
                };
                let result = match intent {
                    crate::conversation_input::ChannelExecutionIntent::Chat => {
                        let binding = item.projection.payload.domain_plugin.clone().map(|plugin| {
                            use crate::tasks::task_runtime::{
                                RunTurnBinding, RunTurnOrigin, TurnVisibility,
                            };
                            RunTurnBinding {
                                domain_plugin: Some(plugin),
                                run_id: None,
                                turn_id: item.attempt.turn_id.clone(),
                                root_message_id: item.attempt.turn_id.clone(),
                                origin: RunTurnOrigin::User,
                                transcript_visibility: TurnVisibility::Visible,
                                expected_resume: None,
                            }
                        });
                        let held_execution = Arc::new(std::sync::Mutex::new(Some(execution)));
                        lease.retain_execution(held_execution.clone());
                        drop(_effect_publication);
                        crate::chat_driver::with_foreground_pool_execution(
                            Arc::downgrade(&held_execution),
                            Box::pin(
                                crate::foreground_turn::drive_foreground_chat_with_source_ingress(
                                    lease,
                                    &agent,
                                    &turn,
                                    resources,
                                    binding,
                                    Some(input_source),
                                    observer,
                                    durable_terminal,
                                ),
                            ),
                        )
                        .await
                    }
                    crate::conversation_input::ChannelExecutionIntent::Resume {
                        expected,
                        driver_continuation_enabled,
                    } => {
                        match channel_resume_dispatch(
                            expected,
                            driver_continuation_enabled,
                            &item.attempt.turn_id,
                        ) {
                            ChannelResumeDispatch::Continuation(binding) => {
                                let held_execution =
                                    Arc::new(std::sync::Mutex::new(Some(execution)));
                                lease.retain_execution(held_execution.clone());
                                drop(_effect_publication);
                                crate::chat_driver::with_foreground_pool_execution(Arc::downgrade(&held_execution),
                                    Box::pin(crate::foreground_turn::drive_foreground_chat_with_source_ingress(
                                        lease, &agent, &turn, resources, Some(binding), Some(input_source), observer, durable_terminal))).await
                            }
                            ChannelResumeDispatch::Planned(expected) => {
                                let launch = match resources.store.clone() {
                                    Some(store) => {
                                        crate::tasks::task_runtime::executor::launch_task_run_resume_with_input(
                                            store,expected,agent,Some(execution),resources.review_integration.clone(),
                                            Some(crate::chat_driver::subagent_trace_sink_for(&resources.sink)),
                                            lease.cancellation_token(),resources.workspace_io_receipt.as_ref().map(|receipt|receipt.invocation()),
                                            item.attempt.turn_id.clone(),input_source.snapshot,
                                        ).await.map_err(crate::chat_driver::ChatTurnError::from)
                                    }
                                    None => Err(crate::chat_driver::ChatTurnError::from(
                                        "TaskRuntime store unavailable".to_string(),
                                    )),
                                };
                                drop(_effect_publication);
                                let outcome = match launch {
                                    Ok(launch) => {
                                        let admitted = self
                                            .app_state
                                            .conversation_inputs()
                                            .accept_task_run_driver(item.attempt.clone())
                                            .await;
                                        if admitted.is_err() {
                                            lease.cancellation_token().cancel();
                                        } else if let Some(control) = resume_control.as_ref() {
                                            control.report(ControlEffectOutcome::Applied).await;
                                        }
                                        match launch.wait().await {
                                            Ok(
                                                crate::tasks::task_runtime::RunOutcome::Completed,
                                            ) if admitted.is_ok() => {
                                                crate::chat_driver::TurnOutcome::Completed
                                            }
                                            Ok(
                                                crate::tasks::task_runtime::RunOutcome::Cancelled,
                                            ) if admitted.is_ok() => {
                                                crate::chat_driver::TurnOutcome::Cancelled
                                            }
                                            Ok(other) => crate::chat_driver::TurnOutcome::Failed(
                                                echo_agent::error::AgentFailure::message(
                                                    "planned_resume",
                                                    format!(
                                                        "original resume ended with {other:?}; admission={}",
                                                        admitted.is_ok()
                                                    ),
                                                ),
                                            ),
                                            Err(error) if error.is_observation() => {
                                                lease.retain_observation(error.clone());
                                                return Err(error);
                                            }
                                            Err(error) => crate::chat_driver::TurnOutcome::Failed(
                                                echo_agent::error::AgentFailure::message(
                                                    "planned_resume",
                                                    error.to_string(),
                                                ),
                                            ),
                                        }
                                    }
                                    Err(error) if error.is_observation() => {
                                        lease.retain_observation(error.clone());
                                        return Err(error);
                                    }
                                    Err(error) => crate::chat_driver::TurnOutcome::Failed(
                                        echo_agent::error::AgentFailure::message(
                                            "planned_resume",
                                            error.to_string(),
                                        ),
                                    ),
                                };
                                lease
                                    .settle_after(outcome, durable_terminal)
                                    .await
                                    .map(|settlement| settlement.outcome)
                                    .map_err(|error| {
                                        crate::chat_driver::ChatTurnError::from(error.to_string())
                                    })
                            }
                        }
                    }
                };
                if let Some(control) = resume_control {
                    control
                        .report(ControlEffectOutcome::Unknown {
                            reason:
                                "original resume driver did not confirm its launch/claim admission"
                                    .to_string(),
                        })
                        .await;
                }
                result
            });
            let original_driver = async {
                let driven =
                    drive_with_channel_prompts(drive, prompts, prompt_tx, self.hitl.as_ref());
                tokio::pin!(driven);
                let outcome = tokio::select! {
                    biased;
                    _ = self.service_cancel.cancelled() => {
                        driver_cancel.cancel();
                        driven.await
                    }
                    result = &mut driven => result,
                }
                .map_err(input_pump::ChannelInputExecutionError::from_driver_error)?;
                #[cfg(test)]
                trace_channel_boundary("driver.physical-terminal");
                let _ = terminal_tx.send(Ok(outcome));
                Ok(())
            };
            if let Some(deferred) = deferred_weixin {
                // Neither side is spawned or detached. A failed send still
                // drains the renderer and observes the ORIGINAL driver terminal.
                let (result, ()) = futures::join!(
                    original_driver,
                    drive_deferred_weixin_reply(
                        deferred,
                        self.app_state.storage.tool_executions.clone()
                    )
                );
                #[cfg(test)]
                trace_channel_boundary("execute-claimed.return");
                result
            } else {
                original_driver.await
            }
        })
    }

    fn recover_claimed<'a>(
        &'a self,
        item: &'a Self::Item,
        reason: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let lease = item
                .lease
                .lock()
                .map_err(|_| "channel input lease is unavailable".to_string())?
                .take();
            let Some(lease) = lease else {
                return Err("channel input foreground terminal debt remains unresolved".to_string());
            };
            let service = self.app_state.conversation_inputs();
            let attempt = item.attempt.clone();
            let reason = reason.to_string();
            let bound_channel = item.projection.payload.channel_binding.is_some();
            lease
                .settle_after(
                    crate::chat_driver::TurnOutcome::Cancelled,
                    move |_outcome| {
                        let service = service.clone();
                        let attempt = attempt.clone();
                        let reason = reason.clone();
                        async move {
                            let receipt = if bound_channel {
                                service.recovery_required(attempt, reason).await
                            } else {
                                service.deferred(attempt, reason).await
                            };
                            receipt.map(|_| ()).map_err(|error| error.to_string())
                        }
                    },
                )
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
}

#[cfg(feature = "channels")]
impl AppChannelMessageHandler {
    async fn start_input_pump(
        &self,
        address: crate::conversation_input::ConversationInputAddress,
        agent_conversation_id: String,
        cache_id: String,
    ) -> Result<(), String> {
        let wiring = self.pump_wiring()?;
        start_channel_input_pump_with(
            &self.session_coordinator,
            Arc::clone(&self.input_pump),
            &wiring,
            address,
            agent_conversation_id,
            cache_id,
        )
        .await
        .map(|_| ())
    }

    fn pump_wiring(&self) -> Result<ChannelPumpWiring, String> {
        Ok(ChannelPumpWiring {
            retirement: Some(self.runtime_retirement_context()),
            weixin_identity: self.weixin_identity.clone(),
            session_instance: Some(self.session_instance.clone()),
            reply_slot: Some(Arc::clone(&self.input_pump)),
            service_cancel: self
                .service_cancel
                .lock()
                .map_err(|_| "channel service owner unavailable".to_string())?
                .clone()
                .unwrap_or_default(),
            weixin_delivery: self.weixin_delivery.clone(),
            bound_control_runtime: self.bound_control_runtime.clone(),
            active_turns: Arc::clone(&self.active_turns),
            surface_id: ChannelSessionCoordinator::surface_id(&self.session_instance),
            coordinator: Arc::downgrade(&self.session_coordinator),
            app_state: Arc::downgrade(&self.app_state),
            foreground_turns: self.foreground_turns.clone(),
            hitl: Arc::clone(&self.hitl),
            human_loop_provider: Arc::clone(&self.human_loop_provider),
        })
    }

    pub(crate) fn new(
        app_state: Arc<crate::state::AppState>,
        webhook_emitter: Arc<crate::webhook::WebhookEmitter>,
        foreground_turns: ForegroundTurnControl,
        session_instance: echo_agent::channels::ChannelSessionInstance,
        session_coordinator: Arc<ChannelSessionCoordinator>,
    ) -> Self {
        let registration = session_coordinator.register(&session_instance);
        let (pending_retirement, initialization_error) = match registration {
            Ok((_previous_incarnation_id, gate)) => (gate, None),
            Err(error) => (Arc::default(), Some(error)),
        };
        let surface_id = ChannelSessionCoordinator::surface_id(&session_instance);
        let hitl_scope = crate::hitl::HitlRequestScope {
            channel_id: Some(session_instance.channel_id().to_string()),
            channel_conversation_id: Some(session_instance.conversation_id().to_string()),
            sender_id: Some(session_instance.sender_id().to_string()),
            sender_incarnation: Some(session_instance.incarnation_id()),
            ..Default::default()
        };
        let conversation = Self::conversation_id(
            session_instance.channel_id(),
            session_instance.conversation_id(),
            session_instance.sender_id(),
        );
        let agent_conversation =
            Self::agent_conversation_id(&conversation, &session_instance.incarnation_id());
        let hitl_scope_resolver = channel_hitl_scope_resolver(
            &session_coordinator.active_turns,
            &foreground_turns,
            surface_id.clone(),
            agent_conversation,
            hitl_scope.clone(),
        );
        let hitl = Arc::new(
            ChannelHumanLoopProvider::new()
                .with_scope(hitl_scope)
                .with_scope_resolver(hitl_scope_resolver.clone()),
        );
        Self {
            service_cancel: std::sync::Mutex::new(None),
            weixin_identity: None,
            admission: None,
            bound_control_runtime: None,
            bound_workspace_generation: None,
            weixin_delivery: None,
            app_state,
            webhook_emitter,
            human_loop_provider: hitl.clone(),
            hitl,
            hitl_scope_resolver,
            foreground_turns,
            session_instance,
            active_turns: Arc::clone(&session_coordinator.active_turns),
            input_pump: session_coordinator.input_pump(&surface_id),
            session_coordinator,
            initialization_error,
            incarnation_fault: Arc::new(std::sync::Mutex::new(None)),
            pending_retirement,
        }
    }

    fn with_gui_hitl_factory(mut self, factory: crate::hitl::ChannelGuiHitlFactorySlot) -> Self {
        self.human_loop_provider = Arc::new(crate::hitl::ChannelHumanLoopEntry::new(
            self.hitl.clone(),
            factory,
            self.hitl_scope_resolver.clone(),
        ));
        self
    }

    /// Weixin captures the original workspace generation at transport start;
    /// subsequent GUI focus changes cannot reroute controls, HITL or the pump.
    pub(super) fn with_bound_control_runtime(
        mut self,
        runtime: crate::state::ScopedChatRuntime,
    ) -> Self {
        // Global is the original process-stable product-data identity. Real
        // workspace generations are supplied by the same Start control capture.
        if runtime.execution_scope().workspace_id() == "global" {
            self.bound_workspace_generation = Some("global".to_string());
        }
        self.bound_control_runtime = Some(runtime);
        self
    }

    pub(super) fn with_weixin_delivery(
        mut self,
        delivery: echo_agent::channels::weixin::WeixinDeliveryHandle,
    ) -> Self {
        self.weixin_delivery = Some(delivery);
        self
    }

    fn with_weixin_credential_identity(mut self, account_id: &str, generation: &str) -> Self {
        self.weixin_identity = Some((account_id.to_string(), generation.to_string()));
        self
    }

    async fn control_runtime(
        &self,
    ) -> Result<crate::state::ScopedChatRuntime, crate::state::ScopedControlError> {
        match &self.bound_control_runtime {
            Some(runtime) if self.session_instance.channel_id() == "weixin" => {
                let generation = self.bound_workspace_generation.as_deref().ok_or_else(|| {
                    crate::state::ScopedControlError::Runtime(
                        "original channel workspace generation unavailable".to_string(),
                    )
                })?;
                let (_publication, control) = self
                    .app_state
                    .channel_control_for_generation(
                        runtime.execution_scope().workspace_id(),
                        generation,
                    )
                    .await
                    .map_err(|error| {
                        crate::state::ScopedControlError::Runtime(error.to_string())
                    })?;
                Ok(control.runtime().clone())
            }
            Some(runtime) => Ok(runtime.clone()),
            None => self.app_state.current_control_runtime().await,
        }
    }

    async fn begin_channel_turn(
        &self,
        surface: ForegroundTurnSurface,
        conversation_id: &str,
        turn_id: String,
    ) -> Result<
        (
            crate::state::ScopedChatRuntime,
            crate::foreground_turn::ForegroundTurnLease,
        ),
        crate::state::ScopedChatTurnError,
    > {
        match &self.bound_control_runtime {
            Some(runtime) => runtime
                .begin_turn(&self.foreground_turns, surface, conversation_id, turn_id)
                .await
                .map(|lease| (runtime.clone(), lease))
                .map_err(crate::state::ScopedChatTurnError::from),
            None => {
                self.app_state
                    .begin_scoped_chat_turn_owned(surface, conversation_id, turn_id)
                    .await
            }
        }
    }

    fn with_admission_gate(mut self, admission: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.admission = Some(admission);
        self
    }

    fn active_turn(&self, surface_id: &ChannelSurfaceIdentity) -> Option<ChannelActiveTurn> {
        self.active_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(surface_id)
            .cloned()
    }

    async fn resolve_active_turn(
        &self,
        surface_id: &ChannelSurfaceIdentity,
        conversation_id: &str,
        agent_conversation_id: &str,
    ) -> Result<Option<ChannelActiveTurn>, String> {
        if let Some(active) = self.active_turn(surface_id) {
            return Ok(Some(active));
        }
        let runtime = self
            .control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let snapshot = channel_snapshot_for_conversation(
            self.foreground_turns
                .snapshots_for_workspace(runtime.execution_scope().workspace_id())
                .map_err(|error| error.to_string())?,
            conversation_id,
        )?;
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        Ok(Some(ChannelActiveTurn {
            runtime,
            agent_conversation_id: agent_conversation_id.to_string(),
            conversation_id: conversation_id.to_string(),
            turn_id: snapshot.root_turn_id,
        }))
    }

    async fn route_live_conversation_input(
        &self,
        active_surface_id: &ChannelSurfaceIdentity,
        conv: &str,
        agent_conv: &str,
        submitted: crate::conversation_input::ConversationInputReceipt,
        reply: input_pump::ChannelInputReplyRoute,
        release_weixin_after_handoff: bool,
    ) -> Result<ChannelLiveInputRoute, String> {
        use crate::conversation_input::ConversationInputPhase;

        let Some(active) = self
            .resolve_active_turn(active_surface_id, conv, agent_conv)
            .await?
        else {
            return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
        };
        let workspace_id = active.runtime.execution_scope().workspace_id().to_string();
        if submitted.identity.address.workspace_id != workspace_id {
            return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
        }
        let snapshots = self
            .foreground_turns
            .snapshots_for_workspace(&workspace_id)
            .map_err(|error| error.to_string())?;
        let Some(snapshot) = channel_snapshot_for_conversation(snapshots, conv)? else {
            Self::clear_active_turn(&self.active_turns, active_surface_id, &active.turn_id);
            return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
        };
        if !channel_snapshot_matches_root(&snapshot, &active.turn_id) {
            Self::clear_active_turn(&self.active_turns, active_surface_id, &active.turn_id);
            return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
        }
        let active_agent_turn_id = channel_steer_target(&snapshot).to_string();
        let service = self.app_state.conversation_inputs();
        let frontier = service
            .list(&submitted.identity.address)
            .await
            .map_err(|error| error.to_string())?;
        if frontier
            .items
            .first()
            .is_none_or(|item| item.receipt.identity != submitted.identity)
        {
            return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
        }
        let projection = match service
            .dispatch_selected(
                submitted.identity.clone(),
                frontier.queue_revision,
                active_agent_turn_id.clone(),
            )
            .await
        {
            Ok(projection) => projection,
            Err(_) => return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply))),
        };
        let attempt = match channel_input_attempt(&projection) {
            Ok(attempt) => attempt,
            Err(error) => {
                let failure = crate::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message("conversation_input", error.clone()),
                );
                let _ = reply
                    .render_tx
                    .send(ChannelRenderEvent::Prompt(format!(
                        "Channel input {} failed before delivery: {error}",
                        submitted.identity.input_id,
                    )))
                    .await;
                let _ = reply.terminal_tx.send(Ok(failure));
                return Ok(ChannelLiveInputRoute::Routed);
            }
        };
        let original_projection = projection.clone();
        let dispatched_input_id = projection.receipt.identity.input_id.clone();
        let steer_text = channel_input_effective_text(&projection.payload);
        let settlement_waiter = match self.foreground_turns.settlement_waiter_scoped(
            &workspace_id,
            ForegroundTurnSurface::Channel,
            conv,
            &active.turn_id,
        ) {
            Ok(waiter) => waiter,
            Err(error) => {
                service
                    .deferred(attempt, error.to_string())
                    .await
                    .map_err(|settle| settle.to_string())?;
                return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
            }
        };
        let flow = match self
            .app_state
            .session
            .product_data_io
            .begin_owned_flow("prepare active channel input")
        {
            Ok(flow) => flow,
            Err(error) => {
                service
                    .deferred(attempt, error.to_string())
                    .await
                    .map_err(|settle| settle.to_string())?;
                return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
            }
        };
        let turn = match prepare_channel_turn(
            ChannelTurnPreparation {
                attachments: projection.payload.attachments,
                execution_root: active.runtime.execution_scope().root().to_path_buf(),
                text: steer_text.clone(),
                conversation_id: conv.to_string(),
                turn_id: active_agent_turn_id.clone(),
                runtime_authored: false,
                workspace_io_receipt: active.runtime.workspace_io_receipt(),
            },
            &flow,
        )
        .await
        {
            Ok(turn) => {
                flow.settle(None);
                turn
            }
            Err(error) => {
                flow.settle(Some(error.clone()));
                service
                    .deferred(attempt, error.clone())
                    .await
                    .map_err(|settle| settle.to_string())?;
                return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
            }
        };
        let Some(pool) = active.runtime.pool() else {
            service
                .deferred(attempt, "active workspace has no AgentPool".to_string())
                .await
                .map_err(|error| error.to_string())?;
            return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
        };
        let execution = match pool.lease_existing(&active.agent_conversation_id).await {
            Ok(Some(execution)) => execution,
            Ok(None) => {
                service
                    .deferred(attempt, "active channel Agent is unavailable".to_string())
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
            }
            Err(error) => {
                service
                    .deferred(attempt, error.to_string())
                    .await
                    .map_err(|settle| settle.to_string())?;
                return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
            }
        };
        let agent = execution.agent();
        let message = match turn.to_message() {
            Ok(message) => message,
            Err(error) => {
                drop(execution);
                service
                    .deferred(attempt, error.to_string())
                    .await
                    .map_err(|settle| settle.to_string())?;
                return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
            }
        };
        let (steer_tx, steer_rx) = tokio::sync::oneshot::channel();
        let (drain_tx, drain_rx) = tokio::sync::oneshot::channel();
        let observer_service = service.clone();
        let observer_attempt = attempt.clone();
        let observer_render_tx = reply.render_tx.clone();
        let observed_phase = Arc::new(std::sync::Mutex::new(None));
        let observer_phase = Arc::clone(&observed_phase);
        let pump_slot = Arc::clone(&self.input_pump);
        let pump_adapter = Arc::new(ChannelInputPumpAdapter {
            retirement: Some(self.runtime_retirement_context()),
            weixin_identity: self.weixin_identity.clone(),
            session_instance: Some(self.session_instance.clone()),
            reply_slot: Some(self.input_pump.clone()),
            service_cancel: self
                .service_cancel
                .lock()
                .map_err(|_| "channel service owner unavailable".to_string())?
                .clone()
                .unwrap_or_default(),
            weixin_delivery: self.weixin_delivery.clone(),
            bound_control_runtime: self.bound_control_runtime.clone(),
            active_turns: self.active_turns.clone(),
            surface_id: ChannelSessionCoordinator::surface_id(&self.session_instance),
            app_state: Arc::clone(&self.app_state),
            foreground_turns: self.foreground_turns.clone(),
            address: submitted.identity.address.clone(),
            agent_conversation_id: agent_conv.to_string(),
            cache_id: Self::cache_user_id(conv, &self.session_instance.incarnation_id()),
            hitl: Arc::clone(&self.hitl),
            human_loop_provider: self.human_loop_provider.clone(),
        });
        let terminal_projector = channel_live_terminal_projector(
            service.clone(),
            attempt.clone(),
            Arc::clone(&observed_phase),
        );
        let terminal_projector = if release_weixin_after_handoff {
            let original_projector = terminal_projector;
            let slot = pump_slot.clone();
            let adapter = pump_adapter.clone();
            let coordinator = self.session_coordinator.clone();
            let identity = submitted.identity.clone();
            let original_inputs = service.clone();
            Arc::new(move |outcome: crate::chat_driver::TurnOutcome| {
                let original = original_projector.clone();
                let slot = slot.clone();
                let adapter = adapter.clone();
                let coordinator = coordinator.clone();
                let identity = identity.clone();
                let original_inputs = original_inputs.clone();
                Box::pin(async move {
                    original(outcome).await?;
                    let replayable = original_inputs
                        .list(&identity.address)
                        .await
                        .map_err(|error| error.to_string())?
                        .items
                        .iter()
                        .find(|entry| entry.receipt.identity == identity)
                        .is_some_and(|entry| entry.receipt.is_dispatchable());
                    if !replayable {
                        slot.forget_reply(&identity)
                            .map_err(|error| error.to_string())?;
                    }
                    match slot.kick() {
                        Ok(input_pump::ChannelInputPumpKick::Started(owner)) => {
                            coordinator.start_input_pump_task(slot, owner, adapter)
                        }
                        Ok(input_pump::ChannelInputPumpKick::Notified)
                        | Err(input_pump::ChannelInputPumpError::ShuttingDown) => Ok(()),
                        Err(error) => Err(error.to_string()),
                    }
                }) as futures::future::BoxFuture<'static, Result<(), String>>
            }) as crate::foreground_turn::ForegroundTerminalProjector
        } else {
            terminal_projector
        };
        if let Err(error) = self.foreground_turns.supervise_input_lifecycle_scoped(
            &workspace_id,
            ForegroundTurnSurface::Channel,
            conv,
            &active_agent_turn_id,
            async move {
                let steer = match steer_rx.await {
                    Ok(steer) => steer,
                    Err(error) => {
                        observer_service
                            .recovery_required(
                                observer_attempt,
                                "Original tracked steer handoff ended without a result".to_string(),
                            )
                            .await
                            .map_err(|persist| persist.to_string())?;
                        *observer_phase
                            .lock()
                            .map_err(|_| "channel live input phase is unavailable".to_string())? =
                            Some(ConversationInputPhase::RecoveryRequired);
                        return Err(error.to_string());
                    }
                };
                let observed = match observer_service
                    .observe_durable_steer_through_drain(observer_attempt, steer)
                    .await
                {
                    Ok(observed) => observed,
                    Err(error) => {
                        *observer_phase
                            .lock()
                            .map_err(|_| "channel live input phase is unavailable".to_string())? =
                            Some(ConversationInputPhase::RecoveryRequired);
                        return Err(error.to_string());
                    }
                };
                *observer_phase
                    .lock()
                    .map_err(|_| "channel live input phase is unavailable".to_string())? =
                    Some(observed.phase);
                let _ = observer_render_tx
                    .send(ChannelRenderEvent::Prompt(format!(
                        "Channel input {} is {}.",
                        dispatched_input_id,
                        channel_input_phase_label(observed.phase),
                    )))
                    .await;
                let _ = drain_tx.send(observed);
                Ok(())
            },
            terminal_projector,
        ) {
            service
                .deferred(attempt, error.to_string())
                .await
                .map_err(|settle| settle.to_string())?;
            return Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)));
        }
        if release_weixin_after_handoff {
            match self
                .input_pump
                .register_reply(submitted.identity.clone(), reply)
            {
                Ok(()) | Err(input_pump::ChannelInputPumpError::ReplyAlreadyRegistered) => {}
                Err(error) => return Err(error.to_string()),
            }
            let steer = self
                .app_state
                .steer_user_input_for_active_turn(
                    &self.foreground_turns,
                    crate::conversation_input::ConversationInputSource::Channel,
                    &original_projection,
                    &agent,
                    message,
                )
                .await;
            if matches!(&steer, Ok(Ok(_))) {
                let _ = self
                    .app_state
                    .record_user_steer_for_active_turn(
                        &workspace_id,
                        conv,
                        &active_agent_turn_id,
                        &steer_text,
                    )
                    .await;
            }
            drop(execution);
            // The existing supervisor now owns the exact result and records
            // MailboxAccepted/Drained/Deferred/RecoveryRequired later. Do not
            // wait for drain: the active root may be waiting for phone HITL.
            return hand_weixin_steer_to_original_owner(steer_tx, steer);
        }
        let steer = self
            .app_state
            .steer_user_input_for_active_turn(
                &self.foreground_turns,
                crate::conversation_input::ConversationInputSource::Channel,
                &original_projection,
                &agent,
                message,
            )
            .await;
        if matches!(&steer, Ok(Ok(_)))
            && let Err(error) = self
                .app_state
                .record_user_steer_for_active_turn(
                    &workspace_id,
                    conv,
                    &active_agent_turn_id,
                    &steer_text,
                )
                .await
        {
            tracing::debug!(%error, "channel user steer was not bound to its TaskRun");
        }
        drop(execution);
        let _ = steer_tx.send(steer);
        let observed = drain_rx.await.map_err(|error| error.to_string())?;
        match observed.phase {
            ConversationInputPhase::Drained | ConversationInputPhase::TurnSettled => {
                self.session_coordinator.start_post_settlement_input_task(
                    settlement_waiter,
                    reply.terminal_tx,
                    pump_slot,
                    pump_adapter,
                )?;
                Ok(ChannelLiveInputRoute::Routed)
            }
            ConversationInputPhase::Deferred => {
                Ok(ChannelLiveInputRoute::PumpPending(Box::new(reply)))
            }
            ConversationInputPhase::RecoveryRequired => {
                let failure = crate::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "channel_input_recovery",
                        observed.reason.unwrap_or_else(|| {
                            "channel input requires explicit recovery".to_string()
                        }),
                    ),
                );
                let _ = reply.terminal_tx.send(Ok(failure));
                Ok(ChannelLiveInputRoute::Routed)
            }
            ConversationInputPhase::Cancelled => {
                let _ = reply
                    .terminal_tx
                    .send(Ok(crate::chat_driver::TurnOutcome::Cancelled));
                Ok(ChannelLiveInputRoute::Routed)
            }
            ConversationInputPhase::Persisted
            | ConversationInputPhase::AttemptStarted
            | ConversationInputPhase::MailboxAccepted => {
                service
                    .recovery_required(
                        attempt,
                        format!(
                            "tracked steer observer returned incomplete phase {}",
                            channel_input_phase_label(observed.phase)
                        ),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                let failure = crate::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "channel_input_recovery",
                        "channel input did not reach a safe replay boundary",
                    ),
                );
                let _ = reply.terminal_tx.send(Ok(failure));
                Ok(ChannelLiveInputRoute::Routed)
            }
        }
    }

    fn publish_active_turn(
        &self,
        surface_id: &ChannelSurfaceIdentity,
        conversation_id: &str,
        agent_conversation_id: &str,
        runtime: &crate::state::ScopedChatRuntime,
        turn_id: &str,
    ) -> Result<(), String> {
        publish_channel_active_turn(
            &self.active_turns,
            surface_id,
            conversation_id,
            agent_conversation_id,
            runtime,
            turn_id,
        )
    }

    fn clear_active_turn(
        active_turns: &std::sync::Mutex<HashMap<ChannelSurfaceIdentity, ChannelActiveTurn>>,
        surface_id: &ChannelSurfaceIdentity,
        turn_id: &str,
    ) {
        let mut active = active_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if channel_active_generation_matches(
            active.get(surface_id).map(|entry| entry.turn_id.as_str()),
            turn_id,
        ) {
            active.remove(surface_id);
        }
    }

    fn runtime_retirement_context(&self) -> Arc<ChannelRuntimeRetirementContext> {
        Arc::new(ChannelRuntimeRetirementContext {
            app_state: self.app_state.clone(),
            foreground_turns: self.foreground_turns.clone(),
            active_turns: self.active_turns.clone(),
            pending_retirement: self.pending_retirement.clone(),
        })
    }
    async fn settle_pending_retirement_for_runtime(
        &self,
        runtime: &crate::state::ScopedChatRuntime,
        conversation_id: &str,
        agent_conversation_id: &str,
        surface_id: &ChannelSurfaceIdentity,
    ) -> Result<(), String> {
        self.runtime_retirement_context()
            .settle_pending_retirement_for_runtime(
                runtime,
                conversation_id,
                agent_conversation_id,
                surface_id,
                true,
            )
            .await
    }
    fn record_runtime_owner(
        &self,
        runtime: &crate::state::ScopedChatRuntime,
        conversation: &str,
        runtime_state: &str,
    ) {
        self.runtime_retirement_context().record_runtime_owner(
            runtime,
            conversation,
            runtime_state,
            &self.session_instance.incarnation_id(),
        );
    }
    fn pending_runtime_obligations(&self) -> Vec<ChannelRuntimeObligation> {
        self.runtime_retirement_context()
            .pending_runtime_obligations()
    }
    async fn clear_runtime_incarnation(
        runtime: &crate::state::ScopedChatRuntime,
        conversation_id: &str,
        runtime_state_id: &str,
    ) -> Result<bool, String> {
        Self::clear_runtime_incarnation_stores(
            runtime.conversation_store(),
            runtime.runtime_state_store(),
            conversation_id,
            runtime_state_id,
        )
        .await
    }

    async fn clear_runtime_incarnation_stores(
        conversation_store: Option<Arc<dyn echo_agent::memory::ConversationStore>>,
        runtime_state_store: Option<Arc<dyn echo_agent::state::RuntimeStateStore>>,
        conversation_id: &str,
        runtime_state_id: &str,
    ) -> Result<bool, String> {
        let Some(runtime_state_store) = runtime_state_store else {
            return Ok(false);
        };
        let indexed = runtime_state_store
            .runtime_state_ids(conversation_id)
            .await
            .map_err(|error| error.to_string())?;
        if !indexed
            .iter()
            .any(|indexed_id| indexed_id == runtime_state_id)
        {
            return Ok(false);
        }
        let conversation_store = conversation_store.ok_or_else(|| {
            "ConversationStore is unavailable for exact channel runtime cleanup".to_string()
        })?;
        let receipt = echo_agent::state::clear_persisted_runtime_incarnation(
            conversation_store.as_ref(),
            runtime_state_store.as_ref(),
            conversation_id,
            runtime_state_id,
        )
        .await
        .map_err(|error| error.to_string())?;
        if runtime_state_id != conversation_id {
            let authority = conversation_store
                .get_projection_authority(runtime_state_id)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(authority) = authority {
                let delete = echo_agent::memory::ManagedConversationDelete::prepare(
                    runtime_state_id,
                    authority.epoch,
                )
                .map_err(|error| error.to_string())?;
                let operation_id = delete.operation_id.clone();
                let deleted = conversation_store
                    .delete_managed_conversation(delete)
                    .await
                    .map_err(|error| error.to_string())?;
                if deleted.operation_id != operation_id
                    || !matches!(
                        deleted.status,
                        echo_agent::memory::ManagedConversationDeleteStatus::Deleted
                            | echo_agent::memory::ManagedConversationDeleteStatus::AlreadyDeleted
                    )
                {
                    return Err(format!(
                        "channel runtime transcript deletion did not settle: {:?}",
                        deleted.status
                    ));
                }
            } else {
                conversation_store
                    .delete_conversation(runtime_state_id)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(receipt.checkpoint_removed)
    }

    fn session_fingerprint(channel_id: &str, chat_id: &str, sender_id: &str) -> String {
        let identity = serde_json::json!([channel_id, chat_id, sender_id]).to_string();
        format!("{:x}", Sha256::digest(identity.as_bytes()))
    }

    /// Stable sender-scoped product identity for journal, TaskRun, and UI.
    fn conversation_id(channel_id: &str, chat_id: &str, sender_id: &str) -> String {
        format!(
            "channel:sha256:{}",
            Self::session_fingerprint(channel_id, chat_id, sender_id)
        )
    }

    /// Ephemeral AgentPool/checkpoint identity for one model-context incarnation.
    fn agent_conversation_id(conversation_id: &str, incarnation_id: &str) -> String {
        let identity = serde_json::json!([conversation_id, incarnation_id]).to_string();
        format!(
            "channel-runtime:sha256:{:x}",
            Sha256::digest(identity.as_bytes())
        )
    }

    /// Full control-surface identity matching the framework session boundary.
    fn active_surface_identity(
        channel_id: &str,
        chat_id: &str,
        sender_id: &str,
    ) -> ChannelSurfaceIdentity {
        ChannelSurfaceIdentity {
            channel_id: channel_id.to_string(),
            chat_id: chat_id.to_string(),
            sender_id: sender_id.to_string(),
        }
    }

    /// Provider cache identity isolated with the model-context incarnation.
    fn cache_user_id(conversation_id: &str, incarnation_id: &str) -> String {
        let identity = serde_json::json!([conversation_id, incarnation_id]).to_string();
        format!("im-{:x}", Sha256::digest(identity.as_bytes()))
    }

    fn generation_receipts(
        runtime: &crate::state::ScopedChatRuntime,
    ) -> Result<crate::foreground_turn::ForegroundExecutionReceipts, String> {
        let task_generation = runtime
            .task_runtime()
            .as_ref()
            .map(|store| store.lease_foreground_generation())
            .transpose()
            .map_err(|error| format!("TaskRuntime generation is unavailable: {error}"))?;
        let mut receipts =
            crate::foreground_turn::ForegroundExecutionReceipts::new(task_generation);
        if let Some(integration) = runtime.review_integration().as_ref() {
            let memory_generation = integration
                .lease_generation()
                .map_err(|error| format!("Memory generation is unavailable: {error}"))?;
            receipts.retain(memory_generation);
        }
        Ok(receipts)
    }

    async fn current_task_run(
        store: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
        conv: &str,
        requested_run_id: Option<&str>,
    ) -> Result<
        (
            Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
            crate::tasks::task_runtime::RunStateSnapshot,
        ),
        String,
    > {
        let store = store.ok_or_else(|| "TaskRuntime store is unavailable".to_string())?;
        let requested_run_id = requested_run_id
            .filter(|run_id| !run_id.trim().is_empty())
            .map(str::to_string);
        let conversation_id = conv.to_string();
        let snapshot = Self::task_runtime_io(store.clone(), "load channel TaskRun", move |store| {
            let run = match requested_run_id {
                Some(run_id) => store.get_run(&run_id)?,
                None => store.latest_run_for_conversation(&conversation_id)?,
            }
            .ok_or_else(|| {
                crate::tasks::task_runtime::StoreError::InvalidPlan(
                    "No TaskRun was found for this conversation.".to_string(),
                )
            })?;
            if run.conversation_id != conversation_id {
                return Err(crate::tasks::task_runtime::StoreError::InvalidPlan(
                    format!("TaskRun {} belongs to another conversation.", run.run_id),
                ));
            }
            store.get_run_state(&run.run_id)?.ok_or_else(|| {
                crate::tasks::task_runtime::StoreError::InvalidPlan(format!(
                    "TaskRun {} has no event projection.",
                    run.run_id
                ))
            })
        })
        .await?;
        Ok((store, snapshot))
    }

    async fn task_runtime_io<T, F>(
        store: Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
        operation: &'static str,
        function: F,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(
                Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
            ) -> Result<T, crate::tasks::task_runtime::StoreError>
            + Send
            + 'static,
    {
        crate::tasks::task_runtime::TaskRuntimeOperation::new(store)
            .run_store(operation, function)
            .await
            .map_err(|error| error.to_string())
    }

    fn agent_router_command_response<'a>(
        &'a self,
        message: &'a str,
        conversation_id: &'a str,
        control: Option<ControlEffectObserver>,
    ) -> futures::future::BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let trimmed = message.trim();
            let (command, arguments) = trimmed
                .split_once(char::is_whitespace)
                .unwrap_or((trimmed, ""));
            match command {
                "/agent-list" => Some(
                    render_agent_command_result(
                        &control,
                        crate::text_commands::agent_router::list_agent_endpoints_result(Some(
                            &self.app_state,
                        ))
                        .await,
                    )
                    .await,
                ),
                "/agent-send" => {
                    let mut parts = arguments.splitn(3, char::is_whitespace);
                    let (Some(workspace_id), Some(target_conversation_id), Some(text)) =
                        (parts.next(), parts.next(), parts.next())
                    else {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Rejected {
                                reason: "Agent control arguments are incomplete".to_string(),
                            },
                        )
                        .await;
                        return Some(
                            "Usage: /agent-send <workspace-id> <conversation-id> <message>"
                                .to_string(),
                        );
                    };
                    if text.trim().is_empty() {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Rejected {
                                reason: "Agent control arguments are incomplete".to_string(),
                            },
                        )
                        .await;
                        return Some(
                            "Usage: /agent-send <workspace-id> <conversation-id> <message>"
                                .to_string(),
                        );
                    }
                    let from = match self
                        .app_state
                        .current_agent_address(Some(conversation_id))
                        .await
                    {
                        Ok(address) => address,
                        Err(error) => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::NotAdmitted {
                                    reason: "Agent source lookup did not admit a send".to_string(),
                                },
                            )
                            .await;
                            return Some(format!("Agent source resolution failed: {error}"));
                        }
                    };
                    Some(
                        render_agent_command_result(
                            &control,
                            crate::text_commands::agent_router::send_agent_text_result(
                                Some(&self.app_state),
                                from,
                                workspace_id,
                                target_conversation_id,
                                text,
                            )
                            .await,
                        )
                        .await,
                    )
                }
                "/agent-status" => {
                    let mut parts = arguments.split_whitespace();
                    let (Some(workspace_id), Some(target_conversation_id)) =
                        (parts.next(), parts.next())
                    else {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Rejected {
                                reason: "Agent control arguments are incomplete".to_string(),
                            },
                        )
                        .await;
                        return Some(
                            "Usage: /agent-status <workspace-id> <conversation-id> [message-id]"
                                .to_string(),
                        );
                    };
                    Some(
                        render_agent_command_result(
                            &control,
                            crate::text_commands::agent_router::agent_delivery_status_result(
                                Some(&self.app_state),
                                workspace_id,
                                target_conversation_id,
                                parts
                                    .next()
                                    .map(str::trim)
                                    .filter(|value| !value.is_empty()),
                            )
                            .await,
                        )
                        .await,
                    )
                }
                "/agent-group" => {
                    let args = arguments.split_whitespace().collect::<Vec<_>>();
                    Some(
                        render_agent_command_result(
                            &control,
                            crate::text_commands::agent_router::execute_agent_group_command_result(
                                Some(&self.app_state),
                                &args,
                            )
                            .await,
                        )
                        .await,
                    )
                }
                _ => None,
            }
        })
    }

    fn task_run_command_response<'a>(
        &'a self,
        message: &'a str,
        conv: &'a str,
        control: Option<ControlEffectObserver>,
    ) -> futures::future::BoxFuture<'a, Option<ChannelTaskRunControl>> {
        Box::pin(async move {
            let mut parts = message.split_whitespace();
            let command = parts.next()?;
            if !is_task_run_control_command(command) {
                return None;
            }
            let runtime = match self.control_runtime().await {
                Ok(runtime) => runtime,
                Err(error) => {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::NotAdmitted {
                            reason: "Task control runtime was unavailable before dispatch"
                                .to_string(),
                        },
                    )
                    .await;
                    return Some(ChannelTaskRunControl::Reply(format!(
                        "Workspace control runtime is unavailable: {error}"
                    )));
                }
            };
            let task_runtime = runtime.task_runtime();
            if matches!(
                command,
                "/subagent-message" | "/subagent-followup" | "/subagent-interrupt"
            ) {
                let (usage, instruction_required) = match command {
                    "/subagent-message" => (
                        crate::text_commands::task_run_control::SUBAGENT_MESSAGE_USAGE,
                        true,
                    ),
                    "/subagent-followup" => (
                        crate::text_commands::task_run_control::SUBAGENT_FOLLOWUP_USAGE,
                        true,
                    ),
                    "/subagent-interrupt" => (
                        crate::text_commands::task_run_control::SUBAGENT_INTERRUPT_USAGE,
                        false,
                    ),
                    _ => return None,
                };
                let values = parts.collect::<Vec<_>>();
                let parsed =
                    match crate::text_commands::task_run_control::parse_subagent_control_args(
                        &values,
                        usage,
                        instruction_required,
                    ) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason: "Task control arguments are invalid".to_string(),
                                },
                            )
                            .await;
                            return Some(ChannelTaskRunControl::Reply(error));
                        }
                    };
                let (store, _) = match Self::current_task_run(
                    task_runtime.clone(),
                    conv,
                    Some(&parsed.identity.run_id),
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::NotAdmitted {
                                reason:
                                    "Task control parsing or target lookup did not admit an operation"
                                        .to_string(),
                            },
                        )
                        .await;
                        return Some(ChannelTaskRunControl::Reply(error));
                    }
                };
                let service = crate::tasks::task_runtime::SubagentControlService::new(store);
                let result = match command {
                    "/subagent-message" => {
                        let Some(instruction) = parsed.instruction.as_deref() else {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason: "Subagent control requires an instruction".to_string(),
                                },
                            )
                            .await;
                            return Some(ChannelTaskRunControl::Reply(format!("Usage: {usage}")));
                        };
                        service
                            .send_message(
                                parsed.identity,
                                instruction,
                                crate::tasks::task_runtime::SubagentControlActorSource::Channel,
                            )
                            .await
                    }
                    "/subagent-followup" => {
                        let Some(instruction) = parsed.instruction.as_deref() else {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason: "Subagent control requires an instruction".to_string(),
                                },
                            )
                            .await;
                            return Some(ChannelTaskRunControl::Reply(format!("Usage: {usage}")));
                        };
                        service
                            .queue_guidance_async(
                                parsed.identity,
                                instruction.to_string(),
                                crate::tasks::task_runtime::SubagentControlActorSource::Channel,
                            )
                            .await
                    }
                    "/subagent-interrupt" => {
                        service
                            .interrupt_subagent(
                                parsed.identity,
                                crate::tasks::task_runtime::SubagentControlActorSource::Channel,
                            )
                            .await
                    }
                    _ => return None,
                };
                let reply = match result {
                    Ok(receipt) => {
                        // Ack this registered control request, never target task completion.
                        report_channel_control(
                            &control,
                            if receipt.status
                                == crate::tasks::task_runtime::SubagentControlStatus::Rejected
                            {
                                ControlEffectOutcome::Rejected {
                                    reason:
                                        "The original Subagent control owner rejected the request"
                                            .to_string(),
                                }
                            } else {
                                ControlEffectOutcome::Applied
                            },
                        )
                        .await;
                        format!(
                            "Subagent command {} is {}{}.",
                            receipt.identity.command_id,
                            receipt.status.as_str(),
                            receipt
                                .detail
                                .as_deref()
                                .map(|detail| format!(": {detail}"))
                                .unwrap_or_default()
                        )
                    }
                    Err(error) => {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Unknown {
                                reason:
                                    "The original Subagent control operation did not confirm its result"
                                        .to_string(),
                            },
                        )
                        .await;
                        format!("Subagent control failed: {error}")
                    }
                };
                return Some(ChannelTaskRunControl::Reply(reply));
            }
            if command == "/task-goal" {
                let values = parts.collect::<Vec<_>>();
                let parsed =
                    match crate::text_commands::task_run_control::parse_run_goal_update_args(
                        &values,
                    ) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason: "Task control arguments are invalid".to_string(),
                                },
                            )
                            .await;
                            return Some(ChannelTaskRunControl::Reply(error));
                        }
                    };
                let (store, snapshot) = match Self::current_task_run(
                    task_runtime.clone(),
                    conv,
                    parsed.requested_run_id.as_deref(),
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::NotAdmitted {
                                reason:
                                    "Task control parsing or target lookup did not admit an operation"
                                        .to_string(),
                            },
                        )
                        .await;
                        return Some(ChannelTaskRunControl::Reply(error));
                    }
                };
                let run_id = snapshot.run.run_id;
                let reply = match Self::task_runtime_io(
                    store,
                    "update channel TaskRun Goal",
                    move |store| {
                        store.update_run_goal(
                            &run_id,
                            parsed.expected_goal_revision,
                            &parsed.new_goal,
                            &parsed.reason,
                            crate::tasks::task_runtime::RunGoalActorSource::Channel,
                        )
                    },
                )
                .await
                {
                    Ok(run) => {
                        report_channel_control(&control, ControlEffectOutcome::Applied).await;
                        format!(
                            "TaskRun {} Goal updated to revision {}; update its task graph before resuming.",
                            run.run_id, run.goal_revision
                        )
                    }
                    Err(error) => {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Unknown {
                                reason:
                                    "Task Goal mutation did not confirm its original journal result"
                                        .to_string(),
                            },
                        )
                        .await;
                        format!("Unable to update TaskRun Goal: {error}")
                    }
                };
                return Some(ChannelTaskRunControl::Reply(reply));
            }
            if command == "/task-requirements" {
                let requested_run_id = parts.next();
                let (store, snapshot) = match Self::current_task_run(
                    task_runtime.clone(),
                    conv,
                    requested_run_id,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::NotAdmitted {
                                reason:
                                    "Task control parsing or target lookup did not admit an operation"
                                        .to_string(),
                            },
                        )
                        .await;
                        return Some(ChannelTaskRunControl::Reply(error));
                    }
                };
                let run_id = snapshot.run.run_id;
                let reply = match Self::task_runtime_io(
                    store,
                    "load channel completion gate",
                    move |store| store.completion_gate_report(&run_id),
                )
                .await
                {
                    Ok(report) => {
                        report_channel_control(&control, ControlEffectOutcome::Applied).await;
                        format_channel_completion_gate(&report)
                    }
                    Err(error) => {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::NotAdmitted {
                                reason: "The completion gate read did not produce a result"
                                    .to_string(),
                            },
                        )
                        .await;
                        format!("Unable to read completion gate: {error}")
                    }
                };
                return Some(ChannelTaskRunControl::Reply(reply));
            }
            if command == "/task-requirement-skip" {
                let values = parts.collect::<Vec<_>>();
                let parsed =
                    match crate::text_commands::task_run_control::parse_requirement_skip_args(
                        &values,
                    ) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason: "Task control arguments are invalid".to_string(),
                                },
                            )
                            .await;
                            return Some(ChannelTaskRunControl::Reply(error));
                        }
                    };
                let (store, snapshot) = match Self::current_task_run(
                    task_runtime.clone(),
                    conv,
                    parsed.requested_run_id.as_deref(),
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::NotAdmitted {
                                reason:
                                    "Task control parsing or target lookup did not admit an operation"
                                        .to_string(),
                            },
                        )
                        .await;
                        return Some(ChannelTaskRunControl::Reply(error));
                    }
                };
                let run_id = snapshot.run.run_id;
                let reply = match Self::task_runtime_io(
                    store,
                    "skip channel Goal requirement",
                    move |store| {
                        store.skip_goal_requirement(
                            &run_id,
                            parsed.expected_goal_revision,
                            &parsed.requirement_id,
                            &parsed.reason,
                            crate::tasks::task_runtime::RunGoalActorSource::Channel,
                        )
                    },
                )
                .await
                {
                    Ok(report) => {
                        report_channel_control(&control, ControlEffectOutcome::Applied).await;
                        format_channel_completion_gate(&report)
                    }
                    Err(error) => {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Unknown {
                                reason:
                                    "Requirement Skip did not confirm its original journal result"
                                        .to_string(),
                            },
                        )
                        .await;
                        format!("Unable to confirm requirement Skip: {error}")
                    }
                };
                return Some(ChannelTaskRunControl::Reply(reply));
            }
            let (action, budget_values, requested_run_id) = match command {
                "/task-run" => {
                    let action = parts.next().unwrap_or("status");
                    if !matches!(action, "status" | "pause" | "resume" | "cancel" | "budget") {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Rejected {
                                reason: "Task control arguments are incomplete or unsupported"
                                    .to_string(),
                            },
                        )
                        .await;
                        return Some(ChannelTaskRunControl::Reply(
                            "Usage: /task-run [status|pause|resume|cancel] [run-id], or /task-run budget <tokens|none> <seconds|none> [run-id]".to_string(),
                        ));
                    }
                    if action == "budget" {
                        let Some(tokens) = parts.next() else {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason: "Task control arguments are incomplete or unsupported"
                                        .to_string(),
                                },
                            )
                            .await;
                            return Some(ChannelTaskRunControl::Reply(
                                "Usage: /task-run budget <tokens|none> <seconds|none> [run-id]"
                                    .to_string(),
                            ));
                        };
                        let Some(time) = parts.next() else {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason: "Task control arguments are incomplete or unsupported"
                                        .to_string(),
                                },
                            )
                            .await;
                            return Some(ChannelTaskRunControl::Reply(
                                "Usage: /task-run budget <tokens|none> <seconds|none> [run-id]"
                                    .to_string(),
                            ));
                        };
                        (action, Some((tokens, time)), parts.next())
                    } else {
                        (action, None, parts.next())
                    }
                }
                "/task-status" => ("status", None, parts.next()),
                "/task-pause" => ("pause", None, parts.next()),
                "/task-resume" => ("resume", None, parts.next()),
                "/task-cancel" => ("cancel", None, parts.next()),
                "/task-budget" => {
                    let Some(tokens) = parts.next() else {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Rejected {
                                reason: "Task control arguments are incomplete or unsupported"
                                    .to_string(),
                            },
                        )
                        .await;
                        return Some(ChannelTaskRunControl::Reply(
                            "Usage: /task-budget <tokens|none> <seconds|none> [run-id]".to_string(),
                        ));
                    };
                    let Some(time) = parts.next() else {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Rejected {
                                reason: "Task control arguments are incomplete or unsupported"
                                    .to_string(),
                            },
                        )
                        .await;
                        return Some(ChannelTaskRunControl::Reply(
                            "Usage: /task-budget <tokens|none> <seconds|none> [run-id]".to_string(),
                        ));
                    };
                    ("budget", Some((tokens, time)), parts.next())
                }
                _ => return None,
            };
            let (store, snapshot) =
                match Self::current_task_run(task_runtime, conv, requested_run_id).await {
                    Ok(value) => value,
                    Err(error) => {
                        report_channel_control(
                        &control,
                        ControlEffectOutcome::NotAdmitted {
                            reason:
                                "Task control parsing or target lookup did not admit an operation"
                                    .to_string(),
                        },
                    )
                    .await;
                        return Some(ChannelTaskRunControl::Reply(error));
                    }
                };
            let run_id = snapshot.run.run_id.clone();
            let reply = match action {
                "status" => {
                    report_channel_control(&control, ControlEffectOutcome::Applied).await;
                    format_channel_task_run_status(&snapshot)
                }
                "pause" => {
                    let owned_run_id = run_id.clone();
                    match Self::task_runtime_io(
                        store.clone(),
                        "pause channel TaskRun",
                        move |store| store.request_pause(&owned_run_id),
                    )
                    .await
                    {
                        Ok(true) => {
                            report_channel_control(&control, ControlEffectOutcome::Applied).await;
                            format!("TaskRun {run_id} paused.")
                        }
                        Ok(false) => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason: "The original TaskRun owner found no pausable run"
                                        .to_string(),
                                },
                            )
                            .await;
                            format!("TaskRun {run_id} is not actively pausable.")
                        }
                        Err(error) => {
                            report_channel_control(&control, ControlEffectOutcome::Unknown { reason: "TaskRun pause request did not confirm its original journal result".to_string() }).await;
                            format!("Unable to pause TaskRun {run_id}: {error}")
                        }
                    }
                }
                "cancel" => {
                    let owned_run_id = run_id.clone();
                    match Self::task_runtime_io(
                        store.clone(),
                        "cancel channel TaskRun",
                        move |store| store.request_cancel(&owned_run_id),
                    )
                    .await
                    {
                        Ok(true) => {
                            report_channel_control(&control, ControlEffectOutcome::Applied).await;
                            format!("TaskRun {run_id} cancelled.")
                        }
                        Ok(false) => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason: "The original TaskRun is already terminal".to_string(),
                                },
                            )
                            .await;
                            format!("TaskRun {run_id} is already terminal.")
                        }
                        Err(error) => {
                            report_channel_control(&control, ControlEffectOutcome::Unknown { reason: "TaskRun cancel request did not confirm its original journal result".to_string() }).await;
                            format!("Unable to cancel TaskRun {run_id}: {error}")
                        }
                    }
                }
                "resume" => {
                    if snapshot.run.status != crate::tasks::task_runtime::TaskRunStatus::Paused {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Rejected {
                                reason: "The original TaskRun is not paused and cannot resume"
                                    .to_string(),
                            },
                        )
                        .await;
                        format!(
                            "TaskRun {run_id} is {}; resume requires paused.",
                            snapshot.run.status.as_str()
                        )
                    } else {
                        return Some(ChannelTaskRunControl::Resume {
                            expected: crate::tasks::task_runtime::TaskRunResumeIdentity::capture(
                                &snapshot,
                            ),
                            continuation_enabled: snapshot
                                .continuation
                                .as_ref()
                                .is_some_and(|continuation| continuation.enabled),
                            runtime: Box::new(runtime),
                        });
                    }
                }
                "budget" => {
                    let Some((token_value, time_value)) = budget_values else {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Rejected {
                                reason: "Task control arguments are incomplete or unsupported"
                                    .to_string(),
                            },
                        )
                        .await;
                        return Some(ChannelTaskRunControl::Reply(
                            "Usage: /task-budget <tokens|none> <seconds|none> [run-id]".to_string(),
                        ));
                    };
                    let budgets = parse_channel_budget(token_value, "token").and_then(|tokens| {
                        parse_channel_budget(time_value, "time").map(|time| (tokens, time))
                    });
                    let budget_input_valid = budgets.is_ok();
                    let result = match budgets {
                        Ok((tokens, time)) => {
                            let owned_run_id = run_id.clone();
                            Self::task_runtime_io(
                                store,
                                "update channel TaskRun budgets",
                                move |store| {
                                    store.update_run_continuation_budgets(
                                        &owned_run_id,
                                        tokens,
                                        time,
                                    )
                                },
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    };
                    match result {
                        Ok(_) => {
                            report_channel_control(&control, ControlEffectOutcome::Applied).await;
                            format!("TaskRun {run_id} budgets updated.")
                        }
                        Err(error) => {
                            report_channel_control(&control, if budget_input_valid {
                                ControlEffectOutcome::Unknown { reason: "TaskRun budget mutation did not confirm its original journal result".to_string() }
                            } else { ControlEffectOutcome::Rejected { reason: "TaskRun budget arguments are invalid".to_string() } }).await;
                            format!("Unable to update TaskRun {run_id} budgets: {error}")
                        }
                    }
                }
                _ => {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::Rejected {
                            reason: "The TaskRun action is unsupported".to_string(),
                        },
                    )
                    .await;
                    "Unsupported TaskRun command.".to_string()
                }
            };
            Some(ChannelTaskRunControl::Reply(reply))
        })
    }

    fn control_command_response<'a>(
        &'a self,
        message: &'a str,
        conv: &'a str,
        agent_conv: &'a str,
        cache_id: &'a str,
        active_surface_id: &'a ChannelSurfaceIdentity,
        control: Option<ControlEffectObserver>,
    ) -> futures::future::BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let mut parts = message.trim().splitn(2, char::is_whitespace);
            let command = parts.next()?;
            let argument = parts.next().map(str::trim).unwrap_or_default();
            match command {
                "/tools" => {
                    let runtime = match self.control_runtime().await {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::NotAdmitted {
                                    reason: "Tool control runtime was unavailable before dispatch"
                                        .to_string(),
                                },
                            )
                            .await;
                            return Some(format!("Workspace runtime is unavailable: {error}"));
                        }
                    };
                    let result = crate::tool_control::execute_tool_control_command_result(
                        &self.app_state,
                        &runtime.primary_agent(),
                        argument,
                    )
                    .await;
                    Some(match result {
                        Ok(text) => {
                            report_channel_control(&control, ControlEffectOutcome::Applied).await;
                            text
                        }
                        Err(error) => {
                            let effect = match &error {
                                crate::tool_control::ToolControlCommandError::InvalidInput(_) => ControlEffectOutcome::Rejected { reason: "Tool control syntax was rejected before policy dispatch".to_string() },
                                crate::tool_control::ToolControlCommandError::Unavailable(_) => ControlEffectOutcome::NotAdmitted { reason: "The read-only tool control owner did not produce a result".to_string() },
                                crate::tool_control::ToolControlCommandError::Operation(_) => ControlEffectOutcome::Unknown { reason: "The original tool policy owner returned an error without a no-effect proof".to_string() },
                            };
                            report_channel_control(&control, effect).await;
                            error.to_string()
                        }
                    })
                }
                "/workflow" => {
                    let workflows = self.app_state.history.workflows.clone();
                    let argument = argument.to_string();
                    let owner_control = control.clone();
                    // 原控制 future 先放到堆上，避免在准入和原服务 owner 的组合
                    // future 中重复内联，放大默认线程的 poll 栈帧。
                    let operation = Box::pin(async move {
                        let result = workflows.execute_command(&argument).await;
                        match result {
                            Ok(text) => {
                                report_channel_control(
                                    &owner_control,
                                    ControlEffectOutcome::Applied,
                                )
                                .await;
                                text
                            }
                            Err(error) => {
                                use crate::workflow_service::WorkflowServiceError;
                                let effect = match &error {
                                WorkflowServiceError::InvalidDefinition(_) | WorkflowServiceError::NotFound(_) => ControlEffectOutcome::Rejected { reason: "The original workflow owner rejected the definition or target before execution".to_string() },
                                WorkflowServiceError::Unavailable(_) => ControlEffectOutcome::NotAdmitted { reason: "The original workflow service was unavailable before execution".to_string() },
                                _ => ControlEffectOutcome::Unknown { reason: "The original workflow operation did not return a confirmed effect result".to_string() },
                            };
                                report_channel_control(&owner_control, effect).await;
                                format!("Workflow command failed: {error}")
                            }
                        }
                    });
                    if let Some(observer) = control
                        .as_ref()
                        .filter(|observer| observer.is_durable_ingress())
                    {
                        match self
                            .session_coordinator
                            .start_control_task(observer.clone(), operation)
                        {
                            Ok(()) => {
                                observer.defer_response();
                                Some("The original workflow owner is running.".into())
                            }
                            Err(error) => {
                                observer
                                    .report(ControlEffectOutcome::NotAdmitted {
                                        reason: error.clone(),
                                    })
                                    .await;
                                Some(format!("Workflow command was not admitted: {error}"))
                            }
                        }
                    } else {
                        Some(operation.await)
                    }
                }
                "/compact" | "/compress" => {
                    let focus = (!argument.is_empty()).then(|| argument.to_string());
                    let runtime = match self.control_runtime().await {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::NotAdmitted {
                                    reason: "Compression runtime was unavailable before admission"
                                        .to_string(),
                                },
                            )
                            .await;
                            return Some(format!("Workspace runtime is unavailable: {error}"));
                        }
                    };
                    if let Err(error) = self
                        .settle_pending_retirement_for_runtime(
                            &runtime,
                            conv,
                            agent_conv,
                            active_surface_id,
                        )
                        .await
                    {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Unknown {
                                reason:
                                    "Compression retirement/preparation did not settle conclusively"
                                        .to_string(),
                            },
                        )
                        .await;
                        return Some(error);
                    }
                    let turn_id = format!("channel-compression:{}", uuid::Uuid::new_v4());
                    let lease = match runtime
                        .begin_turn(
                            &self.foreground_turns,
                            ForegroundTurnSurface::Channel,
                            conv,
                            turn_id.clone(),
                        )
                        .await
                    {
                        Ok(lease) => lease,
                        Err(error) => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::NotAdmitted {
                                    reason:
                                        "The original foreground owner did not admit compression"
                                            .to_string(),
                                },
                            )
                            .await;
                            return Some(format!("Unable to admit compression: {error}"));
                        }
                    };
                    if let Err(message) = self.publish_active_turn(
                        active_surface_id,
                        conv,
                        agent_conv,
                        &runtime,
                        &turn_id,
                    ) {
                        let settlement = settle_channel_turn_after_input_observers(
                            lease,
                            crate::chat_driver::TurnOutcome::Cancelled,
                        )
                        .await;
                        let typed = if settlement.is_ok() {
                            ControlEffectOutcome::NotAdmitted {
                                reason: "Compression never reached its business operation"
                                    .to_string(),
                            }
                        } else {
                            ControlEffectOutcome::Unknown {
                                reason: "Compression preparation foreground settlement is unknown"
                                    .to_string(),
                            }
                        };
                        report_channel_control(&control, typed).await;
                        return Some(match settlement {
                            Ok(_) => message,
                            Err(error) => {
                                format!("{message}; foreground settlement failed: {error}")
                            }
                        });
                    }
                    let active_owner = ChannelActiveTurnOwner::new(
                        Arc::clone(&self.active_turns),
                        active_surface_id.clone(),
                        turn_id.clone(),
                    );
                    let receipts = match Self::generation_receipts(&runtime) {
                        Ok(receipts) => receipts,
                        Err(message) => {
                            let settlement = settle_channel_turn_after_input_observers(
                                lease,
                                crate::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message(
                                        "workspace_generation",
                                        message.clone(),
                                    ),
                                ),
                            )
                            .await;
                            drop(active_owner);
                            report_channel_control(&control, if settlement.is_ok() {
                                ControlEffectOutcome::NotAdmitted { reason: "Compression preparation did not enter the business operation".to_string() }
                            } else { ControlEffectOutcome::Unknown { reason: "Compression preparation settlement is unknown".to_string() } }).await;
                            return Some(match settlement {
                                Ok(_) => message,
                                Err(error) => {
                                    format!("{message}; foreground settlement failed: {error}")
                                }
                            });
                        }
                    };
                    self.record_runtime_owner(&runtime, conv, agent_conv);
                    let execution = match runtime.agent_for(agent_conv).await {
                        Ok(execution) => execution,
                        Err(error) => {
                            receipts.release_lifo();
                            let message = format!("AgentPool admission failed: {error}");
                            let settlement = settle_channel_turn_after_input_observers(
                                lease,
                                crate::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message(
                                        "agent_pool",
                                        error.to_string(),
                                    ),
                                ),
                            )
                            .await;
                            drop(active_owner);
                            report_channel_control(&control, if settlement.is_ok() {
                                ControlEffectOutcome::NotAdmitted { reason: "Compression preparation did not enter the business operation".to_string() }
                            } else { ControlEffectOutcome::Unknown { reason: "Compression preparation settlement is unknown".to_string() } }).await;
                            return Some(match settlement {
                                Ok(_) => message,
                                Err(error) => {
                                    format!("{message}; foreground settlement failed: {error}")
                                }
                            });
                        }
                    };
                    let agent = execution.agent();
                    configure_channel_agent(&agent, cache_id, self.human_loop_provider.clone())
                        .await;
                    let workspace_io_receipt = runtime.workspace_io_receipt();
                    let app_state = Arc::clone(&self.app_state);
                    let workspace_id = runtime.execution_scope().workspace_id().to_string();
                    let conversation_id = conv.to_string();
                    let agent_conversation_id = agent_conv.to_string();
                    let compression_turn_id = turn_id.clone();
                    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                    let owner_control = control.clone();
                    if let Err(error) =
                        self.foreground_turns
                            .supervise(lease, move |lease| async move {
                                if let Some(observer) = owner_control.as_ref() { observer.defer_response(); }
                                let compression = app_state
                                    .compress_conversation_with_agent(
                                        &workspace_id,
                                        &conversation_id,
                                        &agent_conversation_id,
                                        &compression_turn_id,
                                        &agent,
                                        focus,
                                        workspace_io_receipt,
                                        Some(lease.cancellation_token()),
                                    )
                                    .await;
                                drop(execution);
                                receipts.release_lifo();
                                let compression_completed = compression.is_ok();
                                let (outcome, message) = match compression {
                                    Err(
                                        crate::manual_compression::ManualCompressionError::Cancelled,
                                    ) => (
                                        crate::chat_driver::TurnOutcome::Cancelled,
                                        "Context compression was cancelled.".to_string(),
                                    ),
                                    Ok(receipt) => (
                                        crate::chat_driver::TurnOutcome::Completed,
                                        format!(
                                            "Context compressed: {} -> {} messages, {} tokens saved.",
                                            receipt.messages_before,
                                            receipt.messages_after,
                                            receipt.tokens_saved()
                                        ),
                                    ),
                                    Err(error) => (
                                        crate::chat_driver::TurnOutcome::Failed(
                                            echo_agent::error::AgentFailure::message(
                                                error.code(),
                                                error.to_string(),
                                            ),
                                        ),
                                        format!("Context compression failed: {error}"),
                                    ),
                                };
                                let settlement = settle_channel_turn_after_input_observers(lease, outcome).await;
                                report_channel_control(&owner_control, settled_control_outcome(
                                    compression_completed,
                                    settlement.as_ref().is_ok_and(|outcome| matches!(outcome, crate::chat_driver::TurnOutcome::Completed)),
                                    "The compression operation or its original foreground settlement did not complete conclusively",
                                )).await;
                                let message = match settlement { Ok(_) => message, Err(error) => format!("{message}; foreground settlement failed: {error}") };
                                if let Some(observer) = owner_control.as_ref() { observer.send_response(message.clone()).await; }
                                drop(active_owner);
                                let _delivered = result_tx.send(message);
                            })
                    {
                        Self::clear_active_turn(&self.active_turns, active_surface_id, &turn_id);
                        report_channel_control(&control, ControlEffectOutcome::Unknown { reason: "Original compression supervision did not produce a confirmed owner".to_string() }).await;
                        return Some(format!("Unable to supervise compression: {error}"));
                    }
                    if control
                        .as_ref()
                        .is_some_and(ControlEffectObserver::is_durable_ingress)
                    {
                        if let Some(observer) = control.as_ref() {
                            observer.defer_response();
                        }
                        return Some(
                            "The original control owner is settling this operation.".to_string(),
                        );
                    }
                    Some(result_rx.await.unwrap_or_else(|_| {
                        "Compression owner ended without publishing its terminal result."
                            .to_string()
                    }))
                }
                "/stop" => {
                    let active = match self
                        .resolve_active_turn(active_surface_id, conv, agent_conv)
                        .await
                    {
                        Ok(active) => active,
                        Err(error) => {
                            report_channel_control(&control, ControlEffectOutcome::NotAdmitted { reason: "Stop could not inspect an original active turn before cancellation".to_string() }).await;
                            return Some(format!("Unable to inspect the active turn: {error}"));
                        }
                    };
                    let Some(active) = active else {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Rejected {
                                reason: "There is no original channel turn to stop".to_string(),
                            },
                        )
                        .await;
                        return Some("No active channel turn to stop.".to_string());
                    };
                    let workspace_id = active.runtime.execution_scope().workspace_id();
                    let result = channel_cancel_root(
                        &self.foreground_turns,
                        workspace_id,
                        &active.conversation_id,
                        &active.turn_id,
                    )
                    .await;
                    let barrier_complete = channel_cancel_barrier_complete(
                        &self.foreground_turns,
                        workspace_id,
                        &active.conversation_id,
                        &active.turn_id,
                        &result,
                    );
                    if barrier_complete {
                        Self::clear_active_turn(
                            &self.active_turns,
                            active_surface_id,
                            &active.turn_id,
                        );
                    }
                    let effect = match (&result, barrier_complete) {
                        (Ok(_), true) => ControlEffectOutcome::Applied,
                        (Err(ForegroundTurnError::NoActiveTurn { .. }), true) => ControlEffectOutcome::Rejected {
                            reason: "The original channel turn was already physically settled".to_string(),
                        },
                        _ => ControlEffectOutcome::Unknown { reason: "Original stop cancellation/settlement did not prove the physical barrier".to_string() },
                    };
                    report_channel_control(&control, effect).await;
                    Some(match result {
                        Ok(settlement) => format!(
                            "Turn {} settled as {}.",
                            settlement.turn_id,
                            settlement.outcome.status()
                        ),
                        Err(ForegroundTurnError::NoActiveTurn { .. }) if barrier_complete => {
                            "The channel turn already settled.".to_string()
                        }
                        Err(ForegroundTurnError::NoActiveTurn { .. }) => {
                            "The channel turn is still active; retry /stop.".to_string()
                        }
                        Err(error) => format!("Unable to stop the active turn: {error}"),
                    })
                }
                "/cancel" => {
                    let message = match self.hitl.try_resolve_message("/cancel") {
                        ChannelHumanLoopResolution::NoPending => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason: "No pending HITL request exists".to_string(),
                                },
                            )
                            .await;
                            "No pending approval or input request to cancel.".to_string()
                        }
                        ChannelHumanLoopResolution::Resolved(message) => {
                            report_channel_control(&control, ControlEffectOutcome::Applied).await;
                            message
                        }
                        ChannelHumanLoopResolution::Rejected(message) => {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Rejected {
                                    reason:
                                        "The original HITL provider rejected the cancel request"
                                            .to_string(),
                                },
                            )
                            .await;
                            message
                        }
                    };
                    Some(message)
                }
                "/reset" => {
                    let active = match self
                        .resolve_active_turn(active_surface_id, conv, agent_conv)
                        .await
                    {
                        Ok(active) => active,
                        Err(error) => {
                            report_channel_control(&control, ControlEffectOutcome::NotAdmitted { reason: "Reset could not inspect its original turn before any cancellation".to_string() }).await;
                            return Some(format!("Unable to inspect the active turn: {error}"));
                        }
                    };
                    let runtime = match active.as_ref() {
                        Some(active) => active.runtime.clone(),
                        None => match self.control_runtime().await {
                            Ok(runtime) => runtime,
                            Err(error) => {
                                report_channel_control(
                                    &control,
                                    ControlEffectOutcome::NotAdmitted {
                                        reason:
                                            "Reset runtime was unavailable before any cancellation"
                                                .to_string(),
                                    },
                                )
                                .await;
                                return Some(format!("Workspace runtime is unavailable: {error}"));
                            }
                        },
                    };
                    self.record_runtime_owner(&runtime, conv, agent_conv);
                    ChannelSessionCoordinator::mark_incarnation_pending(
                        &self.pending_retirement,
                        &self.session_instance.incarnation_id(),
                    );
                    let reset_obligations = self.pending_runtime_obligations();
                    let cancellation_attempted = active.is_some();
                    if let Some(active) = active.as_ref() {
                        let result = channel_cancel_root(
                            &self.foreground_turns,
                            active.runtime.execution_scope().workspace_id(),
                            &active.conversation_id,
                            &active.turn_id,
                        )
                        .await;
                        let barrier_complete = channel_cancel_barrier_complete(
                            &self.foreground_turns,
                            active.runtime.execution_scope().workspace_id(),
                            &active.conversation_id,
                            &active.turn_id,
                            &result,
                        );
                        if barrier_complete {
                            Self::clear_active_turn(
                                &self.active_turns,
                                active_surface_id,
                                &active.turn_id,
                            );
                        }
                        if let Err(error) = result
                            && !matches!(error, ForegroundTurnError::NoActiveTurn { .. })
                        {
                            report_channel_control(&control, ControlEffectOutcome::Unknown { reason: "Reset attempted to cancel the original turn but its settlement failed".to_string() }).await;
                            return Some(format!(
                                "Unable to reset before the active turn settles: {error}"
                            ));
                        }
                        if !barrier_complete {
                            report_channel_control(&control, ControlEffectOutcome::Unknown { reason: "Reset attempted cancellation without a confirmed physical barrier".to_string() }).await;
                            return Some(
                                "Unable to reset before the active turn settles; retry /reset."
                                    .to_string(),
                            );
                        }
                    }
                    let reset_turn_id = uuid::Uuid::new_v4().to_string();
                    let reset_lease = match runtime
                        .begin_turn(
                            &self.foreground_turns,
                            ForegroundTurnSurface::Channel,
                            conv,
                            reset_turn_id.clone(),
                        )
                        .await
                    {
                        Ok(lease) => lease,
                        Err(
                            crate::conversation_deletion::ConversationDeletionError::Foreground(
                                ForegroundTurnError::Busy { active_turn_id, .. },
                            ),
                        ) => {
                            report_channel_control(
                                &control,
                                reset_pre_effect_outcome(
                                    cancellation_attempted,
                                    "The original foreground owner did not admit reset",
                                ),
                            )
                            .await;
                            return Some(format!(
                                "Turn {active_turn_id} started before reset admission; stop it and retry."
                            ));
                        }
                        Err(error) => {
                            report_channel_control(
                                &control,
                                reset_pre_effect_outcome(
                                    cancellation_attempted,
                                    "The original foreground owner did not admit reset",
                                ),
                            )
                            .await;
                            return Some(format!("Unable to admit reset: {error}"));
                        }
                    };
                    if let Err(message) = self.publish_active_turn(
                        active_surface_id,
                        conv,
                        agent_conv,
                        &runtime,
                        &reset_turn_id,
                    ) {
                        let settlement = settle_channel_turn_after_input_observers(
                            reset_lease,
                            crate::chat_driver::TurnOutcome::Cancelled,
                        )
                        .await;
                        report_channel_control(
                            &control,
                            if settlement.is_ok() {
                                reset_pre_effect_outcome(
                                    cancellation_attempted,
                                    "Reset did not reach the retirement/rotation operation",
                                )
                            } else {
                                ControlEffectOutcome::Unknown {
                                    reason: "Reset preparation foreground settlement is unknown"
                                        .to_string(),
                                }
                            },
                        )
                        .await;
                        return Some(match settlement {
                            Ok(_) => message,
                            Err(error) => {
                                format!("{message}; foreground settlement failed: {error}")
                            }
                        });
                    }
                    let generation_receipts = match Self::generation_receipts(&runtime) {
                        Ok(receipts) => receipts,
                        Err(message) => {
                            let settlement = settle_channel_turn_after_input_observers(
                                reset_lease,
                                crate::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message(
                                        "workspace_generation",
                                        message.clone(),
                                    ),
                                ),
                            )
                            .await;
                            Self::clear_active_turn(
                                &self.active_turns,
                                active_surface_id,
                                &reset_turn_id,
                            );
                            report_channel_control(&control, if settlement.is_ok() {
                                reset_pre_effect_outcome(cancellation_attempted, "Reset generation admission did not reach the retirement/rotation operation")
                            } else { ControlEffectOutcome::Unknown { reason: "Reset generation foreground settlement is unknown".to_string() } }).await;
                            return Some(match settlement {
                                Ok(_) => message,
                                Err(error) => {
                                    format!("{message}; foreground settlement failed: {error}")
                                }
                            });
                        }
                    };
                    let hitl = Arc::clone(&self.hitl);
                    let session_instance = self.session_instance.clone();
                    let session_coordinator = Arc::clone(&self.session_coordinator);
                    let incarnation_fault = Arc::clone(&self.incarnation_fault);
                    let pending_retirement = Arc::clone(&self.pending_retirement);
                    let active_turns = Arc::clone(&self.active_turns);
                    let reset_turn_id_for_owner = reset_turn_id.clone();
                    let active_surface_id_for_owner = active_surface_id.clone();
                    let active_owner = ChannelActiveTurnOwner::new(
                        Arc::clone(&active_turns),
                        active_surface_id_for_owner.clone(),
                        reset_turn_id_for_owner.clone(),
                    );
                    let app_state = Arc::clone(&self.app_state);
                    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                    let owner_control = control.clone();
                    if let Err(error) =
                        self.foreground_turns
                            .supervise(reset_lease, move |reset_lease| async move {
                                if let Some(observer) = owner_control.as_ref() { observer.defer_response(); }
                                let _active_owner = active_owner;
                                hitl.reject_all("Conversation reset by user").await;
                                let retirement_obligations = reset_obligations.clone();
                                let retirement_gate = Arc::clone(&pending_retirement);
                                let retirement = await_channel_retirement(
                                    reset_lease.cancellation_token(),
                                    async move {
                                        let mut retirement_holds =
                                            Vec::with_capacity(retirement_obligations.len());
                                        for obligation in &retirement_obligations {
                                            let runtime = app_state
                                                .chat_runtime_for_scope(
                                                    &obligation.key.workspace_id,
                                                )
                                                .await
                                                .map_err(|error| error.to_string())?;
                                            let workspace = runtime.workspace_io_receipt();
                                            if workspace.host_generation()
                                                != obligation.key.workspace_generation
                                            {
                                                return Err(format!(
                                                    "workspace {} generation changed before reset cleanup",
                                                    obligation.key.workspace_id
                                                ));
                                            }
                                            let pool = runtime.pool().ok_or_else(|| {
                                                "The recorded workspace has no AgentPool.".to_string()
                                            })?;
                                            let retirement = pool
                                                .begin_conversation_retirement(
                                                    &obligation.key.runtime_state_id,
                                                )
                                                .map_err(|error| error.to_string())?;
                                            pool.drain_conversation_retirement(&retirement)
                                                .await
                                                .map_err(|error| error.to_string())?;
                                            if let Some(recorded) = retirement_gate
                                                .lock()
                                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                                .get_mut(&obligation.key)
                                            {
                                                recorded.phase = ChannelRetirementPhase::GcPending;
                                            }
                                            Self::clear_runtime_incarnation(
                                                &runtime,
                                                &obligation.product_conversation_id,
                                                &obligation.key.runtime_state_id,
                                            )
                                            .await?;
                                            retirement_holds.push((pool, retirement));
                                        }
                                        Ok(retirement_holds)
                                    },
                                )
                                .await;
                                let (outcome, message) = match retirement {
                                    Ok(retirement_holds) => {
                                        match session_coordinator.rotate(
                                            &active_surface_id_for_owner,
                                            &session_instance,
                                        ) {
                                            Ok(_) => {
                                                {
                                                    let mut pending = pending_retirement
                                                        .lock()
                                                        .unwrap_or_else(|poisoned| {
                                                            poisoned.into_inner()
                                                        });
                                                    for obligation in &reset_obligations {
                                                        pending.remove(&obligation.key);
                                                    }
                                                }
                                                drop(retirement_holds);
                                                (
                                                    crate::chat_driver::TurnOutcome::Completed,
                                                    "Conversation reset with a clean model context; product history remains available."
                                                        .to_string(),
                                                )
                                            }
                                            Err(error) => (
                                                {
                                                    *incarnation_fault
                                                        .lock()
                                                        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                                        Some(error.clone());
                                                    crate::chat_driver::TurnOutcome::Failed(
                                                        echo_agent::error::AgentFailure::message(
                                                            "channel_session_incarnation",
                                                            error.clone(),
                                                        ),
                                                    )
                                                },
                                                format!("Conversation reset failed closed after coordinator rejection: {error}"),
                                            ),
                                        }
                                    }
                                    Err(error) => channel_retirement_terminal(error),
                                };
                                generation_receipts.release_lifo();
                                let rotation_completed = matches!(outcome, crate::chat_driver::TurnOutcome::Completed);
                                let settlement = settle_channel_turn_after_input_observers(reset_lease, outcome).await;
                                // Only this supervised original reset owner knows
                                // rotation AND physical foreground settlement.
                                report_channel_control(&owner_control, settled_control_outcome(
                                    rotation_completed,
                                    settlement.as_ref().is_ok_and(|outcome| matches!(outcome, crate::chat_driver::TurnOutcome::Completed)),
                                    "Reset retirement/rotation or its original foreground settlement did not complete conclusively",
                                )).await;
                                let message = match settlement { Ok(_) => message, Err(error) => format!("{message}; foreground settlement failed: {error}") };
                                if let Some(observer) = owner_control.as_ref() { observer.send_response(message.clone()).await; }
                                Self::clear_active_turn(
                                    &active_turns,
                                    &active_surface_id_for_owner,
                                    &reset_turn_id_for_owner,
                                );
                                let _delivered = result_tx.send(message);
                            })
                    {
                        Self::clear_active_turn(&self.active_turns, active_surface_id, &reset_turn_id);
                        report_channel_control(&control, ControlEffectOutcome::Unknown { reason: "The original reset supervision did not produce a confirmed owner".to_string() }).await;
                        return Some(format!("Unable to supervise reset settlement: {error}"));
                    }
                    if control
                        .as_ref()
                        .is_some_and(ControlEffectObserver::is_durable_ingress)
                    {
                        if let Some(observer) = control.as_ref() {
                            observer.defer_response();
                        }
                        return Some(
                            "The original control owner is settling this operation.".to_string(),
                        );
                    }
                    Some(result_rx.await.unwrap_or_else(|_| {
                        "Reset owner ended without publishing its terminal result.".to_string()
                    }))
                }
                _ => None,
            }
        })
    }
}

#[cfg(feature = "channels")]
impl Drop for AppChannelMessageHandler {
    fn drop(&mut self) {
        self.hitl
            .close_now("Channel session ended before the request settled");
    }
}

#[cfg(feature = "channels")]
fn format_channel_task_run_status(
    snapshot: &crate::tasks::task_runtime::RunStateSnapshot,
) -> String {
    let mut lines = vec![
        format!(
            "TaskRun {}: {}",
            snapshot.run.run_id,
            snapshot.run.status.as_str()
        ),
        format!(
            "Goal r{}: {}",
            snapshot.run.goal_revision, snapshot.run.goal
        ),
    ];
    if let Some(continuation) = snapshot.continuation.as_ref() {
        let token_budget = continuation
            .token_budget
            .map(|budget| budget.to_string())
            .unwrap_or_else(|| "unbounded".to_string());
        let tokens_remaining = continuation
            .token_budget
            .map(|budget| budget.saturating_sub(continuation.tokens_used).to_string())
            .unwrap_or_else(|| "unbounded".to_string());
        let time_budget = continuation
            .time_budget_seconds
            .map(|budget| budget.to_string())
            .unwrap_or_else(|| "unbounded".to_string());
        let time_remaining = continuation
            .time_budget_seconds
            .map(|budget| {
                budget
                    .saturating_sub(continuation.time_used_seconds)
                    .to_string()
            })
            .unwrap_or_else(|| "unbounded".to_string());
        lines.push(format!(
            "Turn: {}; compactions: {}; deferred: {}",
            continuation
                .active_turn
                .as_ref()
                .or(continuation.last_turn.as_ref())
                .map(|turn| turn.ordinal.to_string())
                .unwrap_or_else(|| "none".to_string()),
            continuation.compaction_count,
            continuation.deferred
        ));
        lines.push(format!(
            "Tokens: used {}, budget {token_budget}, remaining {tokens_remaining}",
            continuation.tokens_used
        ));
        lines.push(format!(
            "Time seconds: used {}, budget {time_budget}, remaining {time_remaining}",
            continuation.time_used_seconds
        ));
        if let Some(pause) = continuation.pause.as_ref() {
            lines.push(format!(
                "Pause: {}{}",
                pause.reason.as_str(),
                pause
                    .detail
                    .as_ref()
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default()
            ));
        }
    }
    lines.join("\n")
}

#[cfg(feature = "channels")]
fn format_channel_completion_gate(
    report: &crate::tasks::task_runtime::CompletionGateReport,
) -> String {
    let mut lines = vec![format!(
        "Completion gate: Goal r{}, Plan r{} ({})",
        report.goal_revision,
        report.plan_revision,
        if report.ready { "ready" } else { "blocked" }
    )];
    lines.extend(report.requirements.iter().map(|item| {
        format!(
            "[{}] {}: {}",
            item.status.as_str(),
            item.requirement.requirement_id,
            item.requirement.title
        )
    }));
    lines.extend(
        report
            .blockers
            .iter()
            .map(|item| format!("BLOCK {:?}: {}", item.code, item.detail)),
    );
    lines.join("\n")
}

#[cfg(feature = "channels")]
fn is_agent_management_command(message: &str) -> bool {
    matches!(
        message.split_whitespace().next(),
        Some("/trace" | "/analysis" | "/papers")
    )
}

#[cfg(feature = "channels")]
#[allow(clippy::large_enum_variant)]
enum ChannelExtensionInput {
    Request(crate::extension_commands::ExtensionCommandRequest),
    ParseFailure {
        kind: crate::extension_commands::ExtensionKind,
        identity: crate::extension_commands::ExtensionCommandIdentity,
        error: String,
    },
}

#[cfg(feature = "channels")]
fn channel_extension_scope_from_product_data(
    workspace_id: &str,
    workspace_generation: String,
    sender_id: &str,
    sender_incarnation: &str,
) -> Result<crate::extension_commands::ExtensionRequestScope, String> {
    crate::extension_commands::ExtensionRequestScope::new(
        workspace_id,
        workspace_generation,
        Some(sender_id.to_string()),
        Some(sender_incarnation.to_string()),
    )
    .map_err(|error| error.to_string())
}

#[cfg(feature = "channels")]
async fn channel_extension_scope_for_runtime(
    state: &crate::state::AppState,
    runtime: &crate::state::ScopedChatRuntime,
    sender_id: &str,
    sender_incarnation: &str,
) -> Result<crate::extension_commands::ExtensionRequestScope, String> {
    let product_data = state
        .product_data_for_runtime(runtime)
        .await
        .map_err(|error| error.to_string())?;
    channel_extension_scope_from_product_data(
        product_data.workspace_id(),
        product_data.generation(),
        sender_id,
        sender_incarnation,
    )
}

#[cfg(feature = "channels")]
fn parse_channel_extension_input(
    message: &str,
    request_id: &str,
    operation_id: &str,
) -> Result<Option<ChannelExtensionInput>, String> {
    let identity = crate::extension_commands::ExtensionCommandIdentity {
        request_id: request_id.to_string(),
        operation_id: operation_id.to_string(),
    };
    match crate::extension_commands::parse_extension_command(message, identity.clone()) {
        Ok(Some(request)) => Ok(Some(ChannelExtensionInput::Request(request))),
        Ok(None) => Ok(None),
        Err(error) => match error.extension {
            Some(kind) => Ok(Some(ChannelExtensionInput::ParseFailure {
                kind,
                identity,
                error: error.message,
            })),
            None => Err(error.message),
        },
    }
}

#[cfg(feature = "channels")]
struct ChannelManagementJournalSink;

#[cfg(feature = "channels")]
impl crate::chat_driver::ChatSink for ChannelManagementJournalSink {
    fn on_event(&self, _event: crate::chat_driver::ChatDriverEvent) -> bool {
        true
    }
}

#[cfg(feature = "channels")]
fn parse_developer_command(message: &str) -> Result<Option<(String, Vec<String>)>, String> {
    let command_token = message.split_whitespace().next().unwrap_or_default();
    let requested = command_token.trim_start_matches('/');
    let command = if requested == "term" {
        "terminal"
    } else {
        requested
    };
    if !crate::developer_commands::DeveloperCommandRegistry::commands()
        .iter()
        .any(|descriptor| descriptor.name == command)
    {
        return Ok(None);
    }
    let mut parts = shell_words::split(message)
        .map_err(|error| format!("Invalid /{command} arguments: {error}"))?
        .into_iter();
    let _command_token = parts
        .next()
        .ok_or_else(|| "Developer command is missing its slash prefix".to_string())?;
    Ok(Some((command.to_string(), parts.collect())))
}

#[cfg(feature = "channels")]
fn render_channel_reflection_receipt(receipt: &crate::reflection::ReflectionReceipt) -> String {
    receipt.display_message()
}

#[cfg(all(feature = "channels", test))]
mod reflection_adapter_tests {
    #[test]
    fn channel_parser_and_projection_use_the_shared_contract() -> Result<(), String> {
        let parsed = crate::reflection::ReflectionCommand::parse("/reflect")
            .map_err(|error| error.to_string())?;
        assert!(parsed.is_some());
        let receipt = crate::reflection::reflection_receipt_fixture();
        let rendered = super::render_channel_reflection_receipt(&receipt);
        assert!(rendered.contains(&receipt.key));
        assert!(rendered.contains(&receipt.content_summary));
        Ok(())
    }
}

#[cfg(feature = "channels")]
#[async_trait::async_trait]
impl echo_agent::channels::MessageHandler for AppChannelMessageHandler {
    async fn handle(
        &self,
        msg: echo_agent::channels::InboundMessage,
    ) -> echo_agent::error::Result<echo_agent::channels::OutboundMessage> {
        use futures::StreamExt;

        let channel_id = msg.channel_id.clone();
        let to = msg.reply_target().to_string();
        let chat_type = msg.chat_type;
        let mut stream = self.handle_stream(msg).await?;
        let mut reply = String::new();
        while let Some(item) = stream.next().await {
            let message = item?;
            if !reply.is_empty() && !reply.ends_with('\n') {
                reply.push('\n');
            }
            reply.push_str(&message.text);
        }
        let bounded = channel_outbound_chunks(&channel_id, &ChannelOutboundDraft::ordinary(reply))
            .into_iter()
            .next()
            .unwrap_or_default();
        Ok(echo_agent::channels::OutboundMessage::new(
            channel_id, to, chat_type, bounded,
        ))
    }

    fn has_pending_work(&self) -> bool {
        // SessionHandler may call this while the coordinator is admitting an
        // input. Keep the eviction observation side-effect-free and avoid
        // synchronously touching the service JoinSet here; its completed owner
        // tasks are reaped only by the coordinator/control close boundary.
        self.input_pump.has_pending_work()
            || self
                .active_turn(&ChannelSessionCoordinator::surface_id(
                    &self.session_instance,
                ))
                .is_some()
    }

    async fn handle_stream<'a>(
        &'a self,
        msg: echo_agent::channels::InboundMessage,
    ) -> echo_agent::error::Result<
        futures::stream::BoxStream<
            'a,
            echo_agent::error::Result<echo_agent::channels::OutboundMessage>,
        >,
    > {
        self.handle_stream_common(msg, None)
            .await
            .map(ChannelHandlerResponse::into_stream)
    }

    fn bind_durable_ingress(
        &self,
        service_cancel: tokio_util::sync::CancellationToken,
    ) -> echo_agent::error::Result<()> {
        let mut owner = self.service_cancel.lock().map_err(|_| {
            echo_agent::error::ReactError::Other("channel service owner unavailable".to_string())
        })?;
        if owner.is_some() || service_cancel.is_cancelled() {
            return Err(echo_agent::error::ReactError::Other(
                "channel service is already bound or closed".to_string(),
            ));
        }
        *owner = Some(service_cancel.clone());
        drop(owner);
        // The retained wake belongs to this exact bound handler; it becomes
        // reachable only after the original service token exists.
        if let Ok(wiring) = self.pump_wiring() {
            let surface = ChannelSessionCoordinator::surface_id(&self.session_instance);
            self.session_coordinator
                .retain_session_wake(surface, Arc::new(ChannelSessionWake { wiring }));
        }
        Ok(())
    }

    async fn admit_with_ingress(
        &self,
        msg: echo_agent::channels::InboundMessage,
        cancel: tokio_util::sync::CancellationToken,
        receipt: echo_agent::channels::ChannelIngressReceipt,
    ) -> echo_agent::error::Result<()> {
        self.admit_weixin_ingress(msg, cancel, receipt).await
    }

    async fn close(&self) -> echo_agent::error::Result<()> {
        if let Some(cancel) = self
            .service_cancel
            .lock()
            .map_err(|_| {
                echo_agent::error::ReactError::Other(
                    "channel service owner unavailable".to_string(),
                )
            })?
            .as_ref()
        {
            cancel.cancel();
        }
        self.input_pump
            .begin_shutdown()
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
        // Withdraw the same paired IM request before waiting for its original
        // foreground. Otherwise a provider awaiting an answer can delay close.
        self.hitl.close_now("Original channel service is closing");
        let surface = ChannelSessionCoordinator::surface_id(&self.session_instance);
        if let Some(active) = self.active_turn(&surface) {
            let result = channel_cancel_root(
                &self.foreground_turns,
                active.runtime.execution_scope().workspace_id(),
                &active.conversation_id,
                &active.turn_id,
            )
            .await;
            if !channel_cancel_barrier_complete(
                &self.foreground_turns,
                active.runtime.execution_scope().workspace_id(),
                &active.conversation_id,
                &active.turn_id,
                &result,
            ) {
                return Err(echo_agent::error::ReactError::Other(
                    "original channel foreground cancellation is not settled".to_string(),
                ));
            }
        }
        Ok(())
    }

    async fn reply(
        &self,
        _msg: echo_agent::channels::OutboundMessage,
    ) -> echo_agent::error::Result<()> {
        // 实际发送由插件 wrapper（QqMessageHandler / FeishuMessageHandler）的 reply 承担
        //（wrapper 拦截 reply -> send_tx -> IM API）。inner reply 保持 no-op，
        // 与原 AgentChannelHandler::reply 一致。
        Ok(())
    }
}

#[cfg(feature = "channels")]
enum ChannelHandlerResponse<'a> {
    Immediate {
        correlation: input_pump::ChannelReplyCorrelation,
        text: String,
    },
    Stream(
        futures::stream::BoxStream<
            'a,
            echo_agent::error::Result<echo_agent::channels::OutboundMessage>,
        >,
    ),
}

#[cfg(feature = "channels")]
impl<'a> ChannelHandlerResponse<'a> {
    fn into_stream(
        self,
    ) -> futures::stream::BoxStream<
        'a,
        echo_agent::error::Result<echo_agent::channels::OutboundMessage>,
    > {
        use futures::StreamExt;
        match self {
            Self::Stream(stream) => stream,
            Self::Immediate { correlation, text } => channel_outbound_transport(
                futures::stream::once(async move { Ok(ChannelOutboundDraft::ordinary(text)) })
                    .boxed(),
                correlation.channel_id,
                correlation.to,
                correlation.chat_type,
                None,
            ),
        }
    }
}

#[cfg(feature = "channels")]
fn channel_immediate_response<'a>(
    message: &echo_agent::channels::InboundMessage,
    text: impl Into<String>,
) -> ChannelHandlerResponse<'a> {
    ChannelHandlerResponse::Immediate {
        correlation: input_pump::ChannelReplyCorrelation::from_inbound(message),
        text: text.into(),
    }
}

#[cfg(feature = "channels")]
type ChannelDispatchIdentity = (String, String, String, ChannelSurfaceIdentity, String);

#[cfg(feature = "channels")]
impl AppChannelMessageHandler {
    fn handle_stream_common<'a>(
        &'a self,
        mut msg: echo_agent::channels::InboundMessage,
        control: Option<ControlEffectObserver>,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ChannelHandlerResponse<'a>>> {
        // 在共用入口自身分配 future，避免准入/非交互调用的父 poll 帧再内联
        // 整个控制分发状态。原调用者、借用生命周期及物理结算语义保持一致。
        Box::pin(async move {
            if self
                .admission
                .as_ref()
                .is_some_and(|admission| !admission.load(std::sync::atomic::Ordering::Acquire))
            {
                report_channel_control(
                    &control,
                    ControlEffectOutcome::NotAdmitted {
                        reason: "Channel admission is closed before control dispatch".to_string(),
                    },
                )
                .await;
                return Ok(channel_immediate_response(
                    &msg,
                    "Channel admission is closed",
                ));
            }
            if let Some(error) = self.initialization_error.as_ref() {
                report_channel_control(
                    &control,
                    ControlEffectOutcome::NotAdmitted {
                        reason: "Channel initialization did not admit the control".to_string(),
                    },
                )
                .await;
                return Ok(channel_immediate_response(
                    &msg,
                    format!("Channel session initialization failed: {error}"),
                ));
            }
            let incarnation_fault = self
                .incarnation_fault
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if let Some(error) = incarnation_fault {
                report_channel_control(
                    &control,
                    ControlEffectOutcome::NotAdmitted {
                        reason: "The quarantined channel did not admit the control".to_string(),
                    },
                )
                .await;
                return Ok(channel_immediate_response(
                    &msg,
                    format!("Channel session is quarantined: {error}"),
                ));
            }
            if self.session_instance.channel_id() != msg.channel_id
                || self.session_instance.conversation_id() != msg.conversation_id()
                || self.session_instance.sender_id() != msg.sender_id
            {
                report_channel_control(
                    &control,
                    ControlEffectOutcome::Rejected {
                        reason: "The original channel session identity does not match".to_string(),
                    },
                )
                .await;
                return Ok(channel_immediate_response(
                    &msg,
                    "Channel session identity changed after framework routing.",
                ));
            }
            // Only the dedicated reply command bypasses ordinary control admission.
            // Existing developer/steering commands keep their original paths.
            if msg.text.split_whitespace().next() == Some("/reply") {
                match self.hitl.try_resolve_message(&msg.text) {
                    ChannelHumanLoopResolution::Resolved(message) => {
                        report_channel_control(&control, ControlEffectOutcome::Applied).await;
                        return Ok(channel_immediate_response(&msg, message));
                    }
                    ChannelHumanLoopResolution::Rejected(message) => {
                        report_channel_control(
                            &control,
                            ControlEffectOutcome::Rejected {
                                reason: "The original HITL provider rejected this response"
                                    .to_string(),
                            },
                        )
                        .await;
                        return Ok(channel_immediate_response(&msg, message));
                    }
                    ChannelHumanLoopResolution::NoPending => {}
                }
            }
            let conv =
                Self::conversation_id(&msg.channel_id, msg.conversation_id(), &msg.sender_id);
            let incarnation_id = self.session_instance.incarnation_id();
            let agent_conv = Self::agent_conversation_id(&conv, &incarnation_id);
            let active_surface_id = Self::active_surface_identity(
                &msg.channel_id,
                msg.conversation_id(),
                &msg.sender_id,
            );
            let cache_id = Self::cache_user_id(&conv, &incarnation_id);
            let extension_operation_id =
                format!("channel-extension:{}:{}", msg.channel_id, msg.message_id);
            let extension_input = match parse_channel_extension_input(
                &msg.text,
                &msg.message_id,
                &extension_operation_id,
            ) {
                Ok(input) => input,
                Err(error) => {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::Rejected {
                            reason: "Invalid extension control syntax".to_string(),
                        },
                    )
                    .await;
                    return Ok(channel_immediate_response(&msg, error));
                }
            };
            if let Some(extension_input) = extension_input {
                return self
                    .dispatch_channel_extension(
                        msg,
                        control,
                        (
                            conv,
                            incarnation_id,
                            agent_conv,
                            active_surface_id,
                            cache_id,
                        ),
                        extension_input,
                    )
                    .await;
            }
            let reflection_command = match crate::reflection::ReflectionCommand::parse(&msg.text) {
                Ok(command) => command,
                Err(error) => {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::Rejected {
                            reason: "Invalid reflection control syntax".to_string(),
                        },
                    )
                    .await;
                    return Ok(channel_immediate_response(&msg, error.to_string()));
                }
            };
            if reflection_command.is_some() {
                return self
                    .dispatch_channel_reflection(
                        msg,
                        control,
                        (
                            conv,
                            incarnation_id,
                            agent_conv,
                            active_surface_id,
                            cache_id,
                        ),
                    )
                    .await;
            }
            let developer_command = match parse_developer_command(&msg.text) {
                Ok(command) => command,
                Err(error) => {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::Rejected {
                            reason: "Invalid developer control syntax".to_string(),
                        },
                    )
                    .await;
                    return Ok(channel_immediate_response(&msg, error));
                }
            };
            if let Some((command, args)) = developer_command {
                return self
                    .dispatch_channel_developer(msg, control, command, args)
                    .await;
            }
            if let Some(message) = self
                .agent_router_command_response(&msg.text, &conv, control.clone())
                .await
            {
                return Ok(channel_immediate_response(&msg, message));
            }
            let mut resume_task_run = None;
            if let Some(control) = self
                .task_run_command_response(&msg.text, &conv, control.clone())
                .await
            {
                match control {
                    ChannelTaskRunControl::Reply(message) => {
                        return Ok(channel_immediate_response(&msg, message));
                    }
                    ChannelTaskRunControl::Resume {
                        expected,
                        continuation_enabled,
                        runtime,
                    } => {
                        resume_task_run = Some((expected, continuation_enabled, runtime));
                    }
                }
            }
            let explicit_steer = msg.text.trim().starts_with("/steer")
                && msg.text.split_whitespace().next() == Some("/steer");
            if explicit_steer {
                let instruction = msg
                    .text
                    .trim()
                    .strip_prefix("/steer")
                    .map(str::trim)
                    .unwrap_or_default();
                if instruction.is_empty() && msg.attachments.is_empty() {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::Rejected {
                            reason: "Steer requires a nonempty instruction or attachment"
                                .to_string(),
                        },
                    )
                    .await;
                    return Ok(channel_immediate_response(
                        &msg,
                        "Usage: /steer <additional instruction>",
                    ));
                }
                msg.text = instruction.to_string();
            }
            // Product control commands always outrank HITL parsing. `/stop` owns
            // turn cancellation; bare `/cancel` only explains the target-qualified reply.
            if !explicit_steer
                && let Some(message) = self
                    .control_command_response(
                        &msg.text,
                        &conv,
                        &agent_conv,
                        &cache_id,
                        &active_surface_id,
                        control.clone(),
                    )
                    .await
            {
                return Ok(channel_immediate_response(&msg, message));
            }
            if msg.text.split_whitespace().next() == Some("/extract") {
                return self
                    .dispatch_channel_extraction(
                        msg,
                        control,
                        (
                            conv,
                            incarnation_id,
                            agent_conv,
                            active_surface_id,
                            cache_id,
                        ),
                    )
                    .await;
            }
            if !explicit_steer && !is_agent_management_command(&msg.text) {
                match self.hitl.try_resolve_message(&msg.text) {
                    ChannelHumanLoopResolution::Resolved(message) => {
                        return Ok(channel_immediate_response(&msg, message));
                    }
                    ChannelHumanLoopResolution::Rejected(message) => {
                        return Ok(channel_immediate_response(&msg, message));
                    }
                    ChannelHumanLoopResolution::NoPending => {}
                }
            }
            // Management commands use the same exact foreground admission and
            // TaskRuntime -> pool order as chat. They do not mutate the agent when
            // any admission step fails.
            if is_agent_management_command(&msg.text) {
                return self
                    .dispatch_channel_management(
                        msg,
                        control,
                        (
                            conv,
                            incarnation_id,
                            agent_conv,
                            active_surface_id,
                            cache_id,
                        ),
                    )
                    .await;
            }

            self.dispatch_channel_chat_resume(
                msg,
                control,
                (
                    conv,
                    incarnation_id,
                    agent_conv,
                    active_surface_id,
                    cache_id,
                ),
                resume_task_run,
                explicit_steer,
            )
            .await
        })
    }

    fn dispatch_channel_extension<'a>(
        &'a self,
        msg: echo_agent::channels::InboundMessage,
        control: Option<ControlEffectObserver>,
        identity: ChannelDispatchIdentity,
        extension_input: ChannelExtensionInput,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ChannelHandlerResponse<'a>>> {
        Box::pin(async move {
            let (conv, incarnation_id, agent_conv, active_surface_id, _cache_id) = identity;
            let retirement_runtime = match self.control_runtime().await {
                Ok(runtime) => runtime,
                Err(error) => {
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Workspace runtime is unavailable: {error}"),
                    ));
                }
            };
            if let Err(error) = self
                .settle_pending_retirement_for_runtime(
                    &retirement_runtime,
                    &conv,
                    &agent_conv,
                    &active_surface_id,
                )
                .await
            {
                return Ok(channel_immediate_response(&msg, error));
            }
            let turn_id = format!("channel-extension-turn:{}", msg.message_id);
            let (runtime, lease) = match self
                .begin_channel_turn(ForegroundTurnSurface::Channel, &conv, turn_id.clone())
                .await
            {
                Ok(lease) => lease,
                Err(crate::state::ScopedChatTurnError::Conversation(
                    crate::conversation_deletion::ConversationDeletionError::Foreground(
                        ForegroundTurnError::Busy { active_turn_id, .. },
                    ),
                )) => {
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Turn {active_turn_id} is still running; command was not applied."),
                    ));
                }
                Err(error) => {
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Unable to admit the Extension command: {error}"),
                    ));
                }
            };
            if let Err(message) =
                self.publish_active_turn(&active_surface_id, &conv, &agent_conv, &runtime, &turn_id)
            {
                let message = match settle_channel_turn_after_input_observers(
                    lease,
                    crate::chat_driver::TurnOutcome::Cancelled,
                )
                .await
                {
                    Ok(_) => message,
                    Err(error) => format!("{message}; foreground settlement failed: {error}"),
                };
                return Ok(channel_immediate_response(&msg, message));
            }
            let active_owner = ChannelActiveTurnOwner::new(
                Arc::clone(&self.active_turns),
                active_surface_id.clone(),
                turn_id.clone(),
            );
            self.record_runtime_owner(&runtime, &conv, &agent_conv);
            let app_state = self.app_state.clone();
            let owner_control = control.clone();
            let operation_message = msg.clone();
            let operation = move |lease: crate::foreground_turn::ForegroundTurnLease| async move {
                let _active_owner = active_owner;
                let control = owner_control;
                let msg = operation_message;
                let scope = runtime.execution_scope().workspace_id().to_string();
                let receipt = match extension_input {
                    ChannelExtensionInput::Request(mut request) if msg.attachments.is_empty() => {
                        match channel_extension_scope_for_runtime(
                            app_state.as_ref(),
                            &runtime,
                            &msg.sender_id,
                            &incarnation_id,
                        )
                        .await
                        {
                            Ok(request_scope) => {
                                request.scope = Some(request_scope);
                                crate::extension_commands::ExtensionCommandDispatcher::new(
                                    app_state.clone(),
                                )
                                .dispatch(request, Some(runtime.clone()), conv.clone())
                                .await
                            }
                            Err(error) => {
                                crate::extension_commands::ExtensionCommandReceipt::failed(
                                    request.kind(),
                                    request.identity(),
                                    scope.clone(),
                                    error.to_string(),
                                )
                            }
                        }
                    }
                    ChannelExtensionInput::Request(request) => {
                        crate::extension_commands::ExtensionCommandReceipt::failed(
                            request.kind(),
                            request.identity(),
                            scope.clone(),
                            "Channel Extension management commands do not accept attachments",
                        )
                    }
                    ChannelExtensionInput::ParseFailure {
                        kind,
                        identity,
                        error,
                    } => crate::extension_commands::ExtensionCommandReceipt::failed(
                        kind,
                        identity,
                        scope.clone(),
                        error,
                    ),
                };
                let extension_settled = matches!(
                    receipt.status(),
                    crate::extension_commands::ExtensionCommandStatus::Settled
                );
                let message = receipt.display_message();
                let outcome = crate::text_commands::extension_receipt_terminal(&receipt);
                let sink = crate::chat_event_log::bind_surface_chat_sink(
                    crate::chat_event_log::ChatSurface::Channel,
                    Arc::new(ChannelManagementJournalSink),
                    app_state.storage.chat_events.clone(),
                    app_state.storage.tool_executions.clone(),
                    scope,
                    Some(conv.clone()),
                    turn_id,
                );
                if !sink.on_event(crate::chat_driver::ChatDriverEvent::ExtensionReceipt(
                    Box::new(receipt),
                )) {
                    let failure = crate::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "channel_journal",
                            "Channel journal rejected the Extension receipt",
                        ),
                    );
                    let settlement =
                        settle_channel_turn_after_input_observers(lease, failure).await;
                    let message = settlement.err().map_or_else(
                    || "Channel journal rejected the Extension receipt.".to_string(),
                    |error| format!("Channel journal rejected the Extension receipt; foreground settlement failed: {error}"),
                );
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::Unknown {
                            reason:
                                "The original extension receipt/journal safe point did not settle"
                                    .to_string(),
                        },
                    )
                    .await;
                    return message;
                }
                let terminal_delivered =
                    sink.on_event(crate::chat_driver::ChatDriverEvent::TurnStatus {
                        status: outcome.status().to_string(),
                    });
                if !terminal_delivered {
                    let failure = crate::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "channel_journal",
                            "Channel journal rejected the Extension terminal",
                        ),
                    );
                    let settlement =
                        settle_channel_turn_after_input_observers(lease, failure).await;
                    let message = settlement.err().map_or_else(
                    || "Channel journal rejected the Extension terminal.".to_string(),
                    |error| format!("Channel journal rejected the Extension terminal; foreground settlement failed: {error}"),
                );
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::Unknown {
                            reason:
                                "The original extension receipt/journal safe point did not settle"
                                    .to_string(),
                        },
                    )
                    .await;
                    return message;
                }
                let settlement = settle_channel_turn_after_input_observers(lease, outcome).await;
                report_channel_control(&control, settled_control_outcome(extension_settled,
                settlement.as_ref().is_ok_and(|outcome| matches!(outcome, crate::chat_driver::TurnOutcome::Completed)),
                "The original extension did not produce both a Settled receipt and completed foreground settlement",
            )).await;
                match settlement {
                    Ok(_) => message,
                    Err(error) => format!("{message}; foreground settlement failed: {error}"),
                }
            };
            if let Some(observer) = control
                .as_ref()
                .filter(|observer| observer.is_durable_ingress())
            {
                let owner_response = observer.clone();
                match self
                    .foreground_turns
                    .supervise(lease, move |lease| async move {
                        owner_response.defer_response();
                        let message = operation(lease).await;
                        owner_response.send_response(message).await;
                    }) {
                    Ok(()) => {
                        observer.defer_response();
                        return Ok(channel_immediate_response(
                            &msg,
                            "The original Extension owner is running.",
                        ));
                    }
                    Err(error) => {
                        observer
                            .report(ControlEffectOutcome::Unknown {
                                reason: format!(
                                    "Original Extension supervision was not confirmed: {error}"
                                ),
                            })
                            .await;
                        return Ok(channel_immediate_response(
                            &msg,
                            format!("Unable to supervise Extension: {error}"),
                        ));
                    }
                }
            }
            let message = operation(lease).await;
            Ok(channel_immediate_response(&msg, message))
        })
    }

    fn dispatch_channel_reflection<'a>(
        &'a self,
        msg: echo_agent::channels::InboundMessage,
        control: Option<ControlEffectObserver>,
        identity: ChannelDispatchIdentity,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ChannelHandlerResponse<'a>>> {
        Box::pin(async move {
            let (conv, _incarnation_id, agent_conv, _active_surface_id, _cache_id) = identity;
            if !msg.attachments.is_empty() {
                report_channel_control(
                    &control,
                    ControlEffectOutcome::Rejected {
                        reason: "Reflection controls do not accept attachments".to_string(),
                    },
                )
                .await;
                return Ok(channel_immediate_response(
                    &msg,
                    "/reflect does not accept attachments",
                ));
            }
            let runtime = match self.control_runtime().await {
                Ok(runtime) => runtime,
                Err(error) => {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::NotAdmitted {
                            reason: "The original reflection operation was not admitted"
                                .to_string(),
                        },
                    )
                    .await;
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Reflection unavailable: {error}"),
                    ));
                }
            };
            let owner_control = control.clone();
            // 同一反思操作仍由原服务 owner 执行并结算；这里只固定堆上存放，
            // 防止控制分发的嵌套 future 耗尽正常线程栈。
            let operation = Box::pin(async move {
                let execution = match runtime.agent_for(&agent_conv).await {
                    Ok(execution) => execution,
                    Err(error) => {
                        report_channel_control(
                            &owner_control,
                            ControlEffectOutcome::NotAdmitted {
                                reason: "The original reflection operation was not admitted"
                                    .to_string(),
                            },
                        )
                        .await;
                        return format!("Reflection unavailable: {error}");
                    }
                };
                let agent = execution.agent();
                let message = match crate::reflection::reflect_session(
                    &runtime,
                    &agent,
                    Some(&conv),
                )
                .await
                {
                    Ok(receipt) => {
                        report_channel_control(&owner_control, ControlEffectOutcome::Applied).await;
                        render_channel_reflection_receipt(&receipt)
                    }
                    Err(error) => {
                        report_channel_control(&owner_control, ControlEffectOutcome::Unknown {
                        reason: "The original reflection operation returned an error without a no-effect proof".to_string(),
                    }).await;
                        format!("Reflection failed: {error}")
                    }
                };
                drop(execution);
                message
            });
            let message = if let Some(observer) = control
                .as_ref()
                .filter(|observer| observer.is_durable_ingress())
            {
                match self
                    .session_coordinator
                    .start_control_task(observer.clone(), operation)
                {
                    Ok(()) => {
                        observer.defer_response();
                        "The original reflection owner is running.".to_string()
                    }
                    Err(error) => {
                        observer
                            .report(ControlEffectOutcome::NotAdmitted {
                                reason: error.clone(),
                            })
                            .await;
                        format!("Reflection was not admitted: {error}")
                    }
                }
            } else {
                operation.await
            };
            Ok(channel_immediate_response(&msg, message))
        })
    }

    fn dispatch_channel_developer<'a>(
        &'a self,
        msg: echo_agent::channels::InboundMessage,
        control: Option<ControlEffectObserver>,
        command: String,
        args: Vec<String>,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ChannelHandlerResponse<'a>>> {
        Box::pin(async move {
            // Subscribe before dispatch so a fast shell cannot exit before
            // this channel starts observing its output.
            let terminal_events = self.app_state.terminal.subscribe();
            let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            let browser_conversation =
                Self::conversation_id(&msg.channel_id, msg.conversation_id(), &msg.sender_id);
            let registry = crate::developer_commands::DeveloperCommandRegistry::new(
                self.app_state.terminal.clone(),
                Some(self.app_state.clone()),
            )
            .with_browser_conversation_id(browser_conversation);
            return match registry.execute(&command, &arg_refs).await {
                Ok(output) => {
                    // This receipt is the registry's actual registered operation
                    // result, independent of any attached terminal/render stream.
                    report_channel_control(&control, ControlEffectOutcome::Applied).await;
                    if control
                        .as_ref()
                        .is_some_and(ControlEffectObserver::is_durable_ingress)
                    {
                        return Ok(channel_immediate_response(&msg, output.message));
                    }
                    match output.attached_terminal {
                        Some(terminal_id) => {
                            Ok(ChannelHandlerResponse::Stream(channel_terminal_stream(
                                &msg,
                                output.message,
                                terminal_events,
                                terminal_id,
                            )))
                        }
                        None => Ok(channel_immediate_response(&msg, output.message)),
                    }
                }
                Err(error) => {
                    // The registry error does not prove no child/operation was
                    // admitted. Never infer retry safety from its display text.
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::Unknown {
                            reason:
                                "Developer registry returned an error without a no-admission proof"
                                    .to_string(),
                        },
                    )
                    .await;
                    Ok(channel_immediate_response(
                        &msg,
                        format!("/{command} failed: {error}"),
                    ))
                }
            };
        })
    }

    fn dispatch_channel_extraction<'a>(
        &'a self,
        msg: echo_agent::channels::InboundMessage,
        control: Option<ControlEffectObserver>,
        identity: ChannelDispatchIdentity,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ChannelHandlerResponse<'a>>> {
        Box::pin(async move {
            let (conv, _incarnation_id, agent_conv, active_surface_id, cache_id) = identity;
            use crate::structured_extraction::PreparedStructuredExtractionCommand;

            let command = msg
                .text
                .trim()
                .strip_prefix("/extract")
                .map(str::trim)
                .unwrap_or_default();
            let prepared = match self
                .app_state
                .history
                .structured_extraction
                .parse_command(command)
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::Rejected {
                            reason: "Structured extraction syntax is invalid".to_string(),
                        },
                    )
                    .await;
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Structured extraction command failed: {error}"),
                    ));
                }
            };
            match prepared {
                PreparedStructuredExtractionCommand::Examples => {
                    let value = serde_json::to_value(
                        self.app_state.history.structured_extraction.examples(),
                    )
                    .and_then(|value| serde_json::to_string_pretty(&value));
                    report_channel_control(
                        &control,
                        if value.is_ok() {
                            ControlEffectOutcome::Applied
                        } else {
                            ControlEffectOutcome::Rejected {
                                reason:
                                    "The read-only extraction control could not produce a result"
                                        .to_string(),
                            }
                        },
                    )
                    .await;
                    Ok(channel_immediate_response(
                        &msg,
                        value.unwrap_or_else(|error| {
                            format!("Structured extraction serialization failed: {error}")
                        }),
                    ))
                }
                PreparedStructuredExtractionCommand::Validate(schema) => {
                    let value = serde_json::to_value(
                        self.app_state
                            .history
                            .structured_extraction
                            .validate_schema(&schema),
                    )
                    .and_then(|value| serde_json::to_string_pretty(&value));
                    report_channel_control(
                        &control,
                        if value.is_ok() {
                            ControlEffectOutcome::Applied
                        } else {
                            ControlEffectOutcome::Rejected {
                                reason:
                                    "The read-only extraction control could not produce a result"
                                        .to_string(),
                            }
                        },
                    )
                    .await;
                    Ok(channel_immediate_response(
                        &msg,
                        value.unwrap_or_else(|error| {
                            format!("Structured extraction serialization failed: {error}")
                        }),
                    ))
                }
                PreparedStructuredExtractionCommand::Extract(request) => {
                    let runtime = match self.control_runtime().await {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            return Ok(channel_immediate_response(
                                &msg,
                                format!("Workspace runtime is unavailable: {error}"),
                            ));
                        }
                    };
                    if let Err(error) = self
                        .settle_pending_retirement_for_runtime(
                            &runtime,
                            &conv,
                            &agent_conv,
                            &active_surface_id,
                        )
                        .await
                    {
                        return Ok(channel_immediate_response(&msg, error));
                    }
                    let turn_id = format!("channel-extraction:{}", uuid::Uuid::new_v4());
                    let lease = match runtime
                        .begin_turn(
                            &self.foreground_turns,
                            ForegroundTurnSurface::Channel,
                            &conv,
                            turn_id.clone(),
                        )
                        .await
                    {
                        Ok(lease) => lease,
                        Err(error) => {
                            return Ok(channel_immediate_response(
                                &msg,
                                format!("Unable to admit structured extraction: {error}"),
                            ));
                        }
                    };
                    if let Err(message) = self.publish_active_turn(
                        &active_surface_id,
                        &conv,
                        &agent_conv,
                        &runtime,
                        &turn_id,
                    ) {
                        let message = match settle_channel_turn_after_input_observers(
                            lease,
                            crate::chat_driver::TurnOutcome::Cancelled,
                        )
                        .await
                        {
                            Ok(_) => message,
                            Err(error) => {
                                format!("{message}; foreground settlement failed: {error}")
                            }
                        };
                        return Ok(channel_immediate_response(&msg, message));
                    }
                    let active_owner = ChannelActiveTurnOwner::new(
                        Arc::clone(&self.active_turns),
                        active_surface_id.clone(),
                        turn_id,
                    );
                    let receipts = match Self::generation_receipts(&runtime) {
                        Ok(receipts) => receipts,
                        Err(message) => {
                            let settlement = settle_channel_turn_after_input_observers(
                                lease,
                                crate::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message(
                                        "workspace_generation",
                                        message.clone(),
                                    ),
                                ),
                            )
                            .await;
                            let message = match settlement {
                                Ok(_) => message,
                                Err(error) => {
                                    format!("{message}; foreground settlement failed: {error}")
                                }
                            };
                            return Ok(channel_immediate_response(&msg, message));
                        }
                    };
                    self.record_runtime_owner(&runtime, &conv, &agent_conv);
                    let app_state = self.app_state.clone();
                    let human_loop_provider = self.human_loop_provider.clone();
                    let owner_control = control.clone();
                    let agent_conv = agent_conv.clone();
                    let cache_id = cache_id.clone();
                    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                    let durable_control = control
                        .as_ref()
                        .is_some_and(ControlEffectObserver::is_durable_ingress);
                    let operation = move |lease: crate::foreground_turn::ForegroundTurnLease| async move {
                        if let Some(observer) = owner_control.as_ref() {
                            observer.defer_response();
                        }
                        let execution = match runtime.agent_for(&agent_conv).await {
                            Ok(execution) => execution,
                            Err(error) => {
                                receipts.release_lifo();
                                let message = format!("AgentPool admission failed: {error}");
                                let settlement = settle_channel_turn_after_input_observers(
                                    lease,
                                    crate::chat_driver::TurnOutcome::Failed(
                                        echo_agent::error::AgentFailure::message(
                                            "agent_pool",
                                            error.to_string(),
                                        ),
                                    ),
                                )
                                .await;
                                drop(active_owner);
                                let message = match settlement {
                                    Ok(_) => message,
                                    Err(error) => {
                                        format!("{message}; foreground settlement failed: {error}")
                                    }
                                };
                                if let Some(observer) = owner_control.as_ref() {
                                    observer.send_response(message.clone()).await;
                                }
                                let _ = result_tx.send(message);
                                return;
                            }
                        };
                        let agent = execution.agent();
                        configure_channel_agent(&agent, &cache_id, human_loop_provider).await;
                        let cancel = lease.cancellation_token();
                        let extraction = await_channel_operation(
                            cancel,
                            app_state
                                .history
                                .structured_extraction
                                .extract(&agent, request),
                        )
                        .await;
                        drop(execution);
                        receipts.release_lifo();
                        let extraction_completed = matches!(&extraction, Some(Ok(_)));
                        let (outcome, message) = match extraction {
                            None => (
                                crate::chat_driver::TurnOutcome::Cancelled,
                                "Structured extraction was cancelled.".to_string(),
                            ),
                            Some(Ok(extracted)) => (
                                crate::chat_driver::TurnOutcome::Completed,
                                serde_json::to_string_pretty(&extracted).unwrap_or_else(|error| {
                                    format!("Structured extraction serialization failed: {error}")
                                }),
                            ),
                            Some(Err(error)) => (
                                crate::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message(
                                        error.code(),
                                        error.to_string(),
                                    ),
                                ),
                                format!("Structured extraction failed: {error}"),
                            ),
                        };
                        let settlement =
                            settle_channel_turn_after_input_observers(lease, outcome).await;
                        report_channel_control(&owner_control, settled_control_outcome(extraction_completed,
                    settlement.as_ref().is_ok_and(|outcome| matches!(outcome, crate::chat_driver::TurnOutcome::Completed)),
                    "The original extraction or its foreground settlement did not complete conclusively",
                )).await;
                        let message = match settlement {
                            Ok(_) => message,
                            Err(error) => {
                                format!("{message}; foreground settlement failed: {error}")
                            }
                        };
                        if let Some(observer) = owner_control.as_ref() {
                            observer.send_response(message.clone()).await;
                        }
                        drop(active_owner);
                        let _ = result_tx.send(message);
                    };
                    if durable_control {
                        if let Err(error) = self.foreground_turns.supervise(lease, operation) {
                            report_channel_control(
                                &control,
                                ControlEffectOutcome::Unknown {
                                    reason: "Original extraction supervision was not confirmed"
                                        .to_string(),
                                },
                            )
                            .await;
                            return Ok(channel_immediate_response(
                                &msg,
                                format!("Unable to supervise extraction: {error}"),
                            ));
                        }
                        if let Some(observer) = control.as_ref() {
                            observer.defer_response();
                        }
                        return Ok(channel_immediate_response(
                            &msg,
                            "The original extraction owner is running.",
                        ));
                    }
                    operation(lease).await;
                    let message = result_rx.await.unwrap_or_else(|_| {
                        "Original extraction owner ended without a terminal result.".to_string()
                    });
                    Ok(channel_immediate_response(&msg, message))
                }
            }
        })
    }

    fn dispatch_channel_management<'a>(
        &'a self,
        msg: echo_agent::channels::InboundMessage,
        control: Option<ControlEffectObserver>,
        identity: ChannelDispatchIdentity,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ChannelHandlerResponse<'a>>> {
        Box::pin(async move {
            let (conv, _incarnation_id, agent_conv, active_surface_id, cache_id) = identity;
            let retirement_runtime = match self.control_runtime().await {
                Ok(runtime) => runtime,
                Err(error) => {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::NotAdmitted {
                            reason: "Management workspace lookup refused before business dispatch"
                                .to_string(),
                        },
                    )
                    .await;
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Workspace runtime is unavailable: {error}"),
                    ));
                }
            };
            if let Err(error) = self
                .settle_pending_retirement_for_runtime(
                    &retirement_runtime,
                    &conv,
                    &agent_conv,
                    &active_surface_id,
                )
                .await
            {
                report_channel_control(&control, ControlEffectOutcome::Unknown {
                    reason: "Original retirement owner did not confirm settlement before management dispatch".to_string(),
                }).await;
                return Ok(channel_immediate_response(&msg, error));
            }
            let command_id = uuid::Uuid::new_v4().to_string();
            let (runtime, lease) = match self
                .begin_channel_turn(ForegroundTurnSurface::Channel, &conv, command_id.clone())
                .await
            {
                Ok(lease) => lease,
                Err(crate::state::ScopedChatTurnError::Conversation(
                    crate::conversation_deletion::ConversationDeletionError::Foreground(
                        ForegroundTurnError::Busy { active_turn_id, .. },
                    ),
                )) => {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::NotAdmitted {
                            reason:
                                "Original management foreground was busy before command dispatch"
                                    .to_string(),
                        },
                    )
                    .await;
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Turn {active_turn_id} is still running; command was not applied."),
                    ));
                }
                Err(error) => {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::NotAdmitted {
                            reason: "Original management foreground refused command admission"
                                .to_string(),
                        },
                    )
                    .await;
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Unable to admit the command: {error}"),
                    ));
                }
            };
            if let Err(message) = self.publish_active_turn(
                &active_surface_id,
                &conv,
                &agent_conv,
                &runtime,
                &command_id,
            ) {
                let settlement = settle_channel_turn_after_input_observers(
                    lease,
                    crate::chat_driver::TurnOutcome::Cancelled,
                )
                .await;
                report_channel_control(
                    &control,
                    if settlement.is_ok() {
                        ControlEffectOutcome::NotAdmitted {
                            reason: "Management surface publication failed before dispatch"
                                .to_string(),
                        }
                    } else {
                        ControlEffectOutcome::Unknown {
                            reason: "Management publication failed with unsettled foreground"
                                .to_string(),
                        }
                    },
                )
                .await;
                let message = match settlement {
                    Ok(_) => message,
                    Err(error) => format!("{message}; foreground settlement failed: {error}"),
                };
                return Ok(channel_immediate_response(&msg, message));
            }
            let active_owner = ChannelActiveTurnOwner::new(
                Arc::clone(&self.active_turns),
                active_surface_id.clone(),
                command_id,
            );
            let generation_receipts = match Self::generation_receipts(&runtime) {
                Ok(receipts) => receipts,
                Err(message) => {
                    let settlement = settle_channel_turn_after_input_observers(
                        lease,
                        crate::chat_driver::TurnOutcome::Failed(
                            echo_agent::error::AgentFailure::message(
                                "workspace_generation",
                                message.clone(),
                            ),
                        ),
                    )
                    .await;
                    report_channel_control(
                        &control,
                        if settlement.is_ok() {
                            ControlEffectOutcome::NotAdmitted {
                                reason: "Management resource admission failed before dispatch"
                                    .to_string(),
                            }
                        } else {
                            ControlEffectOutcome::Unknown {
                                reason:
                                    "Management resource admission failed with unsettled foreground"
                                        .to_string(),
                            }
                        },
                    )
                    .await;
                    let message = match settlement {
                        Ok(_) => message,
                        Err(error) => {
                            format!("{message}; foreground settlement failed: {error}")
                        }
                    };
                    return Ok(channel_immediate_response(&msg, message));
                }
            };
            self.record_runtime_owner(&runtime, &conv, &agent_conv);
            let app_state = self.app_state.clone();
            let human_loop_provider = self.human_loop_provider.clone();
            let owner_control = control.clone();
            let command_text = msg.text.clone();
            let agent_conv = agent_conv.clone();
            let cache_id = cache_id.clone();
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            let durable_control = control
                .as_ref()
                .is_some_and(ControlEffectObserver::is_durable_ingress);
            let operation = move |lease: crate::foreground_turn::ForegroundTurnLease| async move {
                if let Some(observer) = owner_control.as_ref() {
                    observer.defer_response();
                }
                let _active_owner = active_owner;
                let pool_execution = match runtime.agent_for(&agent_conv).await {
                    Ok(execution) => execution,
                    Err(error) => {
                        generation_receipts.release_lifo();
                        let message = format!("AgentPool admission failed: {error}");
                        let settlement = settle_channel_turn_after_input_observers(
                            lease,
                            crate::chat_driver::TurnOutcome::Failed(
                                echo_agent::error::AgentFailure::message(
                                    "agent_pool",
                                    message.clone(),
                                ),
                            ),
                        )
                        .await;
                        report_channel_control(
                            &owner_control,
                            if settlement.is_ok() {
                                ControlEffectOutcome::NotAdmitted {
                                    reason: "Management resource admission failed before dispatch"
                                        .to_string(),
                                }
                            } else {
                                ControlEffectOutcome::Unknown {
                                reason:
                                    "Management resource admission failed with unsettled foreground"
                                        .to_string(),
                            }
                            },
                        )
                        .await;
                        let message = match settlement {
                            Ok(_) => message,
                            Err(error) => {
                                format!("{message}; foreground settlement failed: {error}")
                            }
                        };
                        if let Some(observer) = owner_control.as_ref() {
                            observer.send_response(message.clone()).await;
                        }
                        let _ = result_tx.send(message);
                        return;
                    }
                };
                let agent = pool_execution.agent();
                configure_channel_agent(&agent, &cache_id, human_loop_provider).await;
                let product_data = app_state.product_data_for_runtime(&runtime).await.ok();
                let result =
                    if let Some(result) = channel_trace_response(&agent, &command_text).await {
                        result
                    } else if let Some(result) =
                        channel_analysis_response(product_data.as_ref(), &command_text).await
                    {
                        result
                    } else if let Some(result) =
                        channel_papers_response(product_data.as_ref(), &command_text).await
                    {
                        result
                    } else {
                        Err(crate::text_commands::ManagementCommandError::InvalidInput(
                            "Unsupported channel management command.".to_string(),
                        ))
                    };
                let response = ChannelManagementResponse::from_result(result);
                drop(pool_execution);
                generation_receipts.release_lifo();
                let ChannelManagementResponse {
                    message,
                    terminal,
                    effect,
                } = response;
                let settlement = settle_channel_turn_after_input_observers(lease, terminal).await;
                // A settled foreground only confirms physical ownership. The
                // command's original typed result supplies its business outcome.
                let effect = match (&settlement, effect) {
                (Err(_), _) => ControlEffectOutcome::Unknown {
                    reason: "The original management foreground did not settle physically".to_string(),
                },
                (Ok(crate::chat_driver::TurnOutcome::Completed), effect) => effect,
                (Ok(_), ControlEffectOutcome::Applied) => ControlEffectOutcome::Unknown {
                    reason: "The original management effect completed but foreground completion is uncertain".to_string(),
                },
                (Ok(_), effect) => effect,
            };
                report_channel_control(&owner_control, effect).await;
                let message = match settlement {
                    Ok(_) => message,
                    Err(error) => format!("{message}; foreground settlement failed: {error}"),
                };
                if let Some(observer) = owner_control.as_ref() {
                    observer.send_response(message.clone()).await;
                }
                let _ = result_tx.send(message);
            };
            if durable_control {
                if let Err(error) = self.foreground_turns.supervise(lease, operation) {
                    report_channel_control(
                        &control,
                        ControlEffectOutcome::Unknown {
                            reason: "Original management supervision was not confirmed".to_string(),
                        },
                    )
                    .await;
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Unable to supervise management: {error}"),
                    ));
                }
                if let Some(observer) = control.as_ref() {
                    observer.defer_response();
                }
                return Ok(channel_immediate_response(
                    &msg,
                    "The original management owner is running.",
                ));
            }
            operation(lease).await;
            let message = result_rx.await.unwrap_or_else(|_| {
                "Original management owner ended without a terminal result.".to_string()
            });
            Ok(channel_immediate_response(&msg, message))
        })
    }

    fn dispatch_channel_chat_resume<'a>(
        &'a self,
        msg: echo_agent::channels::InboundMessage,
        control: Option<ControlEffectObserver>,
        identity: ChannelDispatchIdentity,
        resume_task_run: Option<(
            crate::tasks::task_runtime::TaskRunResumeIdentity,
            bool,
            Box<crate::state::ScopedChatRuntime>,
        )>,
        explicit_steer: bool,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ChannelHandlerResponse<'a>>> {
        Box::pin(async move {
            let (conv, _incarnation_id, agent_conv, active_surface_id, cache_id) = identity;
            if msg.channel_id == "weixin"
                && msg
                    .text
                    .split_whitespace()
                    .next()
                    .is_some_and(|token| token.starts_with('/'))
                && !explicit_steer
                && resume_task_run.is_none()
            {
                report_channel_control(
                    &control,
                    ControlEffectOutcome::Rejected {
                        reason: "No supported control branch admitted this request".to_string(),
                    },
                )
                .await;
                return Ok(channel_immediate_response(
                    &msg,
                    "Unknown Weixin control command; no Agent input was admitted.",
                ));
            }

            if channel_resume_rejects_attachments(resume_task_run.is_some(), msg.attachments.len())
            {
                return Ok(channel_immediate_response(
                    &msg,
                    "TaskRun resume does not accept new attachments; send them in a separate turn.",
                ));
            }
            let retirement_runtime = match resume_task_run.as_ref() {
                Some((_, _, runtime)) => runtime.as_ref().clone(),
                None => match self.control_runtime().await {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        return Ok(channel_immediate_response(
                            &msg,
                            format!("Workspace runtime is unavailable: {error}"),
                        ));
                    }
                },
            };
            if resume_task_run.is_some()
                && let Err(error) = self
                    .settle_pending_retirement_for_runtime(
                        &retirement_runtime,
                        &conv,
                        &agent_conv,
                        &active_surface_id,
                    )
                    .await
            {
                return Ok(channel_immediate_response(&msg, error));
            }
            if resume_task_run.is_none() {
                let address = channel_input_address(
                    retirement_runtime.execution_scope().workspace_id(),
                    &conv,
                );
                let input_id = match crate::conversation_input::stable_scoped_input_id(
                    &address,
                    crate::conversation_input::ConversationInputSource::Channel,
                    &msg.message_id,
                ) {
                    Ok(input_id) => input_id,
                    Err(error) => {
                        return Ok(channel_immediate_response(
                            &msg,
                            format!("Unable to identify channel input: {error}"),
                        ));
                    }
                };
                let attachments = msg
                    .attachments
                    .iter()
                    .enumerate()
                    .map(|(index, attachment)| channel_attachment_data(index, attachment))
                    .collect::<Vec<_>>();
                let submitted = match self
                    .app_state
                    .conversation_inputs()
                    .submit(address, input_id, msg.text.clone(), attachments)
                    .await
                {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        return Ok(channel_immediate_response(
                            &msg,
                            format!("Unable to persist channel input: {error}"),
                        ));
                    }
                };
                if !submitted.is_dispatchable() {
                    return Ok(channel_immediate_response(
                        &msg,
                        "这条消息已接收，无需重复发送。",
                    ));
                }
                if let Err(error) = self
                    .settle_pending_retirement_for_runtime(
                        &retirement_runtime,
                        &conv,
                        &agent_conv,
                        &active_surface_id,
                    )
                    .await
                {
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Channel input remains persisted: {error}"),
                    ));
                }
                let (reply_route, reply_receiver) = input_pump::channel_input_reply_route(&msg);
                if self.active_turn(&active_surface_id).is_some() {
                    match self
                        .route_live_conversation_input(
                            &active_surface_id,
                            &conv,
                            &agent_conv,
                            submitted.clone(),
                            reply_route,
                            false,
                        )
                        .await
                    {
                        Ok(ChannelLiveInputRoute::WeixinAcceptedPending) => {
                            return Err(echo_agent::error::ReactError::Other(
                                "Unexpected live input disposition".to_string(),
                            ));
                        }
                        Ok(ChannelLiveInputRoute::Routed) => {
                            return Ok(ChannelHandlerResponse::Stream(
                                channel_input_response_stream(
                                    reply_receiver,
                                    self.app_state.storage.tool_executions.clone(),
                                    None,
                                )
                                .await,
                            ));
                        }
                        Ok(ChannelLiveInputRoute::PumpPending(reply_route)) => {
                            if let Err(error) = self
                                .input_pump
                                .register_reply(submitted.identity.clone(), *reply_route)
                            {
                                return Ok(channel_immediate_response(
                                    &msg,
                                    format!(
                                        "Channel input is durable but its response route failed: {error}"
                                    ),
                                ));
                            }
                            if let Err(error) = self
                                .start_input_pump(
                                    submitted.identity.address.clone(),
                                    agent_conv.clone(),
                                    cache_id.clone(),
                                )
                                .await
                            {
                                return Ok(channel_immediate_response(
                                    &msg,
                                    format!(
                                        "Channel input is durable but its pump failed: {error}"
                                    ),
                                ));
                            }
                            return Ok(ChannelHandlerResponse::Stream(
                                channel_input_response_stream(
                                    reply_receiver,
                                    self.app_state.storage.tool_executions.clone(),
                                    None,
                                )
                                .await,
                            ));
                        }
                        Err(error) => {
                            return Ok(channel_immediate_response(
                                &msg,
                                format!("Unable to deliver durable channel input: {error}"),
                            ));
                        }
                    }
                } else {
                    if let Err(error) = self
                        .input_pump
                        .register_reply(submitted.identity.clone(), reply_route)
                    {
                        return Ok(channel_immediate_response(
                            &msg,
                            format!(
                                "Channel input is durable but its response route failed: {error}"
                            ),
                        ));
                    }
                    if let Err(error) = self
                        .start_input_pump(
                            submitted.identity.address.clone(),
                            agent_conv.clone(),
                            cache_id.clone(),
                        )
                        .await
                    {
                        return Ok(channel_immediate_response(
                            &msg,
                            format!("Channel input is durable but its pump failed: {error}"),
                        ));
                    }
                    return Ok(ChannelHandlerResponse::Stream(
                        channel_input_response_stream(
                            reply_receiver,
                            self.app_state.storage.tool_executions.clone(),
                            None,
                        )
                        .await,
                    ));
                }
            }
            let turn_id = uuid::Uuid::new_v4().to_string();
            let admission = match resume_task_run.as_ref() {
                Some((_, _, runtime)) => runtime
                    .begin_turn(
                        &self.foreground_turns,
                        ForegroundTurnSurface::Channel,
                        &conv,
                        turn_id.clone(),
                    )
                    .await
                    .map(|lease| (runtime.as_ref().clone(), lease))
                    .map_err(crate::state::ScopedChatTurnError::from),
                None => {
                    self.begin_channel_turn(ForegroundTurnSurface::Channel, &conv, turn_id.clone())
                        .await
                }
            };
            let (scoped_runtime, foreground_lease) = match admission {
                Ok(admission) => admission,
                Err(crate::state::ScopedChatTurnError::Conversation(
                    crate::conversation_deletion::ConversationDeletionError::Foreground(
                        ForegroundTurnError::Busy { active_turn_id, .. },
                    ),
                )) => {
                    return Ok(channel_immediate_response(
                        &msg,
                        format!(
                            "Turn {active_turn_id} won admission; channel input remains queued."
                        ),
                    ));
                }
                Err(error) => {
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("Foreground turn admission failed: {error}"),
                    ));
                }
            };
            if let Err(message) = self.publish_active_turn(
                &active_surface_id,
                &conv,
                &agent_conv,
                &scoped_runtime,
                &turn_id,
            ) {
                let message = match settle_channel_turn_after_input_observers(
                    foreground_lease,
                    crate::chat_driver::TurnOutcome::Cancelled,
                )
                .await
                {
                    Ok(_) => message,
                    Err(error) => format!("{message}; foreground settlement failed: {error}"),
                };
                return Ok(channel_immediate_response(&msg, message));
            }
            self.record_runtime_owner(&scoped_runtime, &conv, &agent_conv);
            let Some(pool) = scoped_runtime.pool() else {
                let failure = crate::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "agent_pool",
                        "The active workspace has no AgentPool",
                    ),
                );
                let settlement =
                    settle_channel_turn_after_input_observers(foreground_lease, failure).await;
                Self::clear_active_turn(&self.active_turns, &active_surface_id, &turn_id);
                if let Err(error) = settlement {
                    return Ok(channel_immediate_response(
                        &msg,
                        format!("AgentPool unavailable and foreground settlement failed: {error}"),
                    ));
                }
                return Ok(channel_immediate_response(
                    &msg,
                    "The active workspace has no AgentPool.",
                ));
            };
            if let Some((expected, _, _)) = resume_task_run.as_ref() {
                let validation = match scoped_runtime.task_runtime() {
                    Some(store) => {
                        let expected_identity = expected.clone();
                        let run_id = expected.run_id.clone();
                        Self::task_runtime_io(store, "validate channel TaskRun resume", move |store| {
                            let snapshot = store.get_run_state(&run_id)?.ok_or_else(|| {
                                crate::tasks::task_runtime::StoreError::InvalidPlan(
                                    format!("TaskRun '{run_id}' no longer exists"),
                                )
                            })?;
                            expected_identity.validate_resumable(&snapshot).map_err(|error| {
                                crate::tasks::task_runtime::StoreError::InvalidPlan(error)
                            })
                        })
                        .await
                    }
                    None => Err("TaskRuntime store is unavailable".to_string()),
                }
                .and_then(|()| {
                    if expected.workspace_id != scoped_runtime.execution_scope().workspace_id() {
                        Err(format!(
                            "TaskRun '{}' was queued for workspace '{}', but admitted workspace is '{}'",
                            expected.run_id,
                            expected.workspace_id,
                            scoped_runtime.execution_scope().workspace_id()
                        ))
                    } else if expected.conversation_id != conv {
                        Err(format!(
                            "TaskRun '{}' was queued for conversation '{}', but channel conversation is '{}'",
                            expected.run_id, expected.conversation_id, conv
                        ))
                    } else {
                        Ok(())
                    }
                });
                if let Err(detail) = validation {
                    let settlement = settle_channel_turn_after_input_observers(
                        foreground_lease,
                        crate::chat_driver::TurnOutcome::Failed(
                            echo_agent::error::AgentFailure::message(
                                "task_run_resume",
                                detail.clone(),
                            ),
                        ),
                    )
                    .await;
                    Self::clear_active_turn(&self.active_turns, &active_surface_id, &turn_id);
                    if let Err(error) = settlement {
                        return Ok(channel_immediate_response(
                            &msg,
                            format!("{detail}; foreground settlement failed: {error}"),
                        ));
                    }
                    return Ok(channel_immediate_response(&msg, detail));
                }
            }
            let stream_cancel = foreground_lease.cancellation_token();
            let text = resume_task_run.as_ref().map_or_else(
                || msg.text.clone(),
                |(expected, _, _)| {
                    format!(
                        "Resume the existing TaskRun {} toward its unchanged Goal. Reload the authoritative TaskRuntime projection and continue the next useful work.",
                        expected.run_id
                    )
                },
            );
            let execution_root = scoped_runtime.execution_scope().root().to_path_buf();
            let workspace_io_receipt = scoped_runtime.workspace_io_receipt();
            // Persist IM attachments into the same durable reference contract as
            // GUI/TUI under the exact immutable execution scope captured at turn
            // admission. PreparedUserTurn then promotes these staging files into
            // conversation/turn-scoped resources shared by main and Subagents.
            let runtime_authored = resume_task_run.is_some();
            let prepared_attachments = Vec::new();
            let prepared_conversation_id = conv.clone();
            let prepared_turn_id = turn_id.clone();
            let prepared_workspace_receipt = workspace_io_receipt.clone();

            let (tx, rx) =
                tokio::sync::mpsc::channel::<ChannelRenderEvent>(CHANNEL_EVENT_QUEUE_CAPACITY);
            let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
            let store = scoped_runtime.task_runtime();
            let execution_scope = scoped_runtime.execution_scope().clone();
            let app_state = self.app_state.clone();
            let turn_preparation_flow = match self
                .app_state
                .session
                .product_data_io
                .begin_owned_flow("prepare channel user turn")
            {
                Ok(flow) => flow,
                Err(error) => {
                    let detail = error.to_string();
                    let failure = crate::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "product_data_admission",
                            detail.clone(),
                        ),
                    );
                    let settlement =
                        settle_channel_turn_after_input_observers(foreground_lease, failure).await;
                    Self::clear_active_turn(&self.active_turns, &active_surface_id, &turn_id);
                    if let Err(settlement_error) = settlement {
                        return Ok(channel_immediate_response(
                            &msg,
                            format!("{detail}; foreground settlement failed: {settlement_error}"),
                        ));
                    }
                    return Ok(channel_immediate_response(&msg, detail));
                }
            };
            let webhook_emitter = self.webhook_emitter.clone();
            let review_integration = scoped_runtime.review_integration();
            let hitl = self.human_loop_provider.clone();
            let prompt_rx = self.hitl.subscribe_prompts();
            let conv_owned = conv.clone();
            let agent_conv_owned = agent_conv.clone();
            let resume_dispatch =
                resume_task_run.map(|(expected_resume, continuation_enabled, _)| {
                    channel_resume_dispatch(expected_resume, continuation_enabled, &turn_id)
                });
            let (planned_resume, explicit_binding) = match resume_dispatch {
                Some(ChannelResumeDispatch::Planned(expected)) => (Some(expected), None),
                Some(ChannelResumeDispatch::Continuation(binding)) => (None, Some(binding)),
                None => (None, None),
            };
            let renderer_cancel = stream_cancel.clone();
            let active_turns = Arc::clone(&self.active_turns);
            let active_surface_id_for_owner = active_surface_id.clone();
            let active_turn_id = turn_id.clone();
            let active_owner = ChannelActiveTurnOwner::new(
                Arc::clone(&active_turns),
                active_surface_id_for_owner.clone(),
                active_turn_id.clone(),
            );
            let driver_turn_id = turn_id.clone();
            let owner_control = control.clone();
            let supervision =
                self.foreground_turns
                    .supervise(foreground_lease, move |foreground_lease| async move {
                        let _active_owner = active_owner;
                        use crate::foreground_turn::drive_foreground_pooled_chat_turn;

                        let renderer: std::sync::Arc<dyn crate::chat_driver::ChatSink> =
                            std::sync::Arc::new(ChannelSurfaceSink::new(tx, renderer_cancel));
                        let sink = crate::chat_event_log::bind_surface_chat_sink(
                            crate::chat_event_log::ChatSurface::Channel,
                            renderer,
                            app_state.storage.chat_events.clone(),
                            app_state.storage.tool_executions.clone(),
                            execution_scope.workspace_id().to_string(),
                            Some(conv_owned.clone()),
                            driver_turn_id.clone(),
                        );
                        let turn = match prepare_channel_turn(
                            ChannelTurnPreparation {
                                attachments: prepared_attachments,
                                execution_root,
                                text,
                                conversation_id: prepared_conversation_id,
                                turn_id: prepared_turn_id,
                                runtime_authored,
                                workspace_io_receipt: prepared_workspace_receipt,
                            },
                            &turn_preparation_flow,
                        )
                        .await
                        {
                            Ok(turn) => {
                                turn_preparation_flow.settle(None);
                                turn
                            }
                            Err(error) => {
                                turn_preparation_flow.settle(Some(error.clone()));
                                tracing::warn!(%error, "channel user-turn preparation failed");
                                let outcome = crate::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message("prepared_turn", error),
                                );
                                let outcome = match settle_channel_turn_after_input_observers(
                                    foreground_lease,
                                    outcome,
                                )
                                .await
                                {
                                    Ok(outcome) => outcome,
                                    Err(error) => {
                                        tracing::error!(%error, "channel preparation settlement failed");
                                        return;
                                    }
                                };
                                let _delivered = terminal_tx.send(Ok(outcome));
                                return;
                            }
                        };
                        let res =
                            std::sync::Arc::new(crate::chat_resources::ChatResources {
                                execution_scope,
                                workspace_io_receipt: Some(workspace_io_receipt),
                                pool: Some(pool.clone()),
                                store,
                                sink,
                                webhook_emitter: Some(webhook_emitter),
                                conv_id: Some(conv_owned.clone()),
                                root_message_id: driver_turn_id,
                                attachments: turn.inline_attachment_refs(),
                                cancel: foreground_lease.cancellation_token(),
                                review_integration,
                                memory_generation: None,
                                human_loop_provider: Some(hitl.clone()),
                            });
                        let configure_cache_id = cache_id;
                        let configure_hitl = hitl;
                        let configure = move |agent| async move {
                            configure_channel_agent(&agent, &configure_cache_id, configure_hitl).await;
                            Ok(())
                        };
                        let outcome = if let Some(expected) = planned_resume {
                            let execution = match pool.acquire(&agent_conv_owned).await {
                                Ok(execution) => execution,
                                Err(error) => {
                                    let outcome = crate::chat_driver::TurnOutcome::Failed(
                                        echo_agent::error::AgentFailure::message(
                                            "agent_pool",
                                            error.to_string(),
                                        ),
                                    );
                                    let outcome = match settle_channel_turn_after_input_observers(
                                        foreground_lease,
                                        outcome,
                                    )
                                    .await
                                    {
                                        Ok(outcome) => outcome,
                                        Err(error) => {
                                            tracing::error!(%error, "planned channel resume settlement failed");
                                            return;
                                        }
                                    };
                                    let _delivered = terminal_tx.send(Ok(outcome));
                                    return;
                                }
                            };
                            let agent = execution.agent();
                            if let Err(error) = configure(agent.clone()).await {
                                let outcome = crate::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message(
                                        "agent_configuration",
                                        error,
                                    ),
                                );
                                match settle_channel_turn_after_input_observers(
                                    foreground_lease,
                                    outcome,
                                )
                                .await
                                {
                                    Ok(outcome) => outcome,
                                    Err(error) => {
                                        tracing::error!(%error, "planned channel configuration settlement failed");
                                        return;
                                    }
                                }
                            } else {
                                let trace_sink =
                                    crate::chat_driver::subagent_trace_sink_for(
                                        &res.sink,
                                    );
                                let launch = match res.store.clone() {
                                Some(store) => {
                                    crate::tasks::task_runtime::launch_task_run_resume(
                                        store,
                                        expected,
                                        agent,
                                        Some(execution),
                                        res.review_integration.clone(),
                                        Some(trace_sink),
                                        foreground_lease.cancellation_token(),
                                        res.workspace_io_receipt
                                            .as_ref()
                                            .map(|receipt| receipt.invocation()),
                                    )
                                    .await
                                    .map_err(crate::chat_driver::ChatTurnError::from)
                                }
                                None => Err("TaskRuntime store is unavailable".into()),
                            };
                                let outcome = match launch {
                                Ok(launch) => match launch.wait().await {
                                    Ok(
                                        crate::tasks::task_runtime::RunOutcome::Completed,
                                    ) => crate::chat_driver::TurnOutcome::Completed,
                                    Ok(
                                        crate::tasks::task_runtime::RunOutcome::Cancelled,
                                    ) => crate::chat_driver::TurnOutcome::Cancelled,
                                    Ok(other) => crate::chat_driver::TurnOutcome::Failed(
                                        echo_agent::error::AgentFailure::message(
                                            "planned_resume",
                                            format!("planned resume ended with {other:?}"),
                                        ),
                                    ),
                                    Err(error) if error.is_observation() => {
                                        foreground_lease.retain_observation(error.clone());
                                        let _ = terminal_tx.send(Err(error));
                                        return;
                                    }
                                    Err(error) => {
                                        crate::chat_driver::TurnOutcome::Failed(
                                            echo_agent::error::AgentFailure::message(
                                                "planned_resume",
                                                error.to_string(),
                                            ),
                                        )
                                    }
                                },
                                Err(error) if error.is_observation() => {
                                    foreground_lease.retain_observation(error.clone());
                                    let _ = terminal_tx.send(Err(error));
                                    return;
                                }
                                Err(error) => crate::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message("planned_resume", error.to_string()),
                                ),
                            };
                                match settle_channel_turn_after_input_observers(
                                    foreground_lease,
                                    outcome,
                                )
                                .await
                                {
                                    Ok(outcome) => outcome,
                                    Err(error) => {
                                        tracing::error!(%error, "planned channel launch settlement failed");
                                        return;
                                    }
                                }
                            }
                        } else {
                            match explicit_binding {
                                Some(binding) => {
                                    match drive_foreground_pooled_chat_turn(
                                        foreground_lease,
                                        pool,
                                        agent_conv_owned,
                                        configure,
                                        &turn,
                                        res,
                                        binding,
                                    )
                                    .await {
                                        Ok(outcome) => outcome,
                                        Err(error) => { let _ = terminal_tx.send(Err(error)); return; }
                                    }
                                }
                                None => {
                                    let outcome = crate::chat_driver::TurnOutcome::Failed(
                                        echo_agent::error::AgentFailure::message(
                                            "task_run_resume",
                                            "TaskRun resume lost its planned or continuation binding",
                                        ),
                                    );
                                    match settle_channel_turn_after_input_observers(
                                        foreground_lease,
                                        outcome,
                                    )
                                    .await
                                    {
                                        Ok(outcome) => outcome,
                                        Err(error) => {
                                            tracing::error!(%error, "channel resume binding settlement failed");
                                            return;
                                        }
                                    }
                                }
                            }
                        };
                        Self::clear_active_turn(
                            &active_turns,
                            &active_surface_id_for_owner,
                            &active_turn_id,
                        );
                        report_channel_control(&owner_control, settled_control_outcome(
                            matches!(outcome, crate::chat_driver::TurnOutcome::Completed),
                            true, // Each branch above returns only after the original foreground settlement.
                            "The original TaskRun resume did not return a completed physical outcome",
                        )).await;
                        let _delivered = terminal_tx.send(Ok(outcome));
                    });
            if let Err(error) = supervision {
                Self::clear_active_turn(&self.active_turns, &active_surface_id, &turn_id);
                return Ok(channel_immediate_response(
                    &msg,
                    format!("Unable to supervise the channel turn: {error}"),
                ));
            }
            // Project the complete shared product stream into channel text.
            // Construct the guard before the generator so dropping an unpolled
            // stream still cancels the lease-owned token.
            let stream_drop_guard = ChannelStreamDropGuard(stream_cancel);
            let event_stream =
                channel_render_event_stream(rx, prompt_rx, terminal_rx, stream_drop_guard);

            // 4. 聚合成逐段 OutboundMessage 流
            let channel_id = msg.channel_id.clone();
            let to = msg.reply_target().to_string();
            let chat_type = msg.chat_type;
            Ok(ChannelHandlerResponse::Stream(
                aggregate_by_sentence_with_repository(
                    event_stream,
                    channel_id,
                    to,
                    chat_type,
                    self.app_state.storage.tool_executions.clone(),
                    None,
                )
                .await,
            ))
        })
    }
}

#[cfg(feature = "channels")]
async fn configure_channel_agent(
    agent: &crate::agent_handle::AgentHandle,
    cache_id: &str,
    hitl: Arc<dyn echo_agent::human_loop::HumanLoopProvider>,
) {
    agent
        .write(|agent| agent.config_mut().set_cache_user_id(cache_id))
        .await;
    agent
        .write_async(|agent| {
            Box::pin(async move {
                agent.set_human_loop_provider_preserving_approvals(hitl);
            })
        })
        .await;
}

#[cfg(feature = "channels")]
async fn channel_trace_response(
    agent: &crate::agent_handle::AgentHandle,
    message: &str,
) -> Option<Result<String, crate::text_commands::ManagementCommandError>> {
    use crate::text_commands::ManagementCommandError;
    let mut parts = message.split_whitespace();
    if parts.next()? != "/trace" {
        return None;
    }
    let store = agent.read(|agent| agent.run_store.clone()).await;
    let Some(store) = store else {
        return Some(Err(ManagementCommandError::NotAdmitted(
            "Run diagnostics are not configured.".to_string(),
        )));
    };
    let diagnostic_id = match parts.next() {
        Some(value) => value.to_string(),
        None => match crate::observability::list_diagnostic_runs(store.as_ref()).await {
            Ok(runs) => match runs.first() {
                Some(run) => run.diagnostic_id.clone(),
                None => return Some(Ok("No durable run diagnostics available.".to_string())),
            },
            Err(error) => {
                return Some(Err(ManagementCommandError::ReadFailed(format!(
                    "Unable to list run diagnostics: {error}"
                ))));
            }
        },
    };
    Some(
        match crate::observability::load_run_diagnostics(store.as_ref(), &diagnostic_id, None).await
        {
            Ok(Some(diagnostics)) => Ok(crate::observability::format_run_diagnostics(&diagnostics)),
            Ok(None) => Err(ManagementCommandError::InvalidInput(format!(
                "Run diagnostics not found: {diagnostic_id}"
            ))),
            Err(error) => Err(ManagementCommandError::ReadFailed(format!(
                "Unable to load run diagnostics: {error}"
            ))),
        },
    )
}

#[cfg(feature = "channels")]
async fn channel_analysis_response(
    product_data: Option<&crate::product_data_io::ScopedProductData>,
    message: &str,
) -> Option<Result<String, crate::text_commands::ManagementCommandError>> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/analysis" {
        return None;
    }
    let args: Vec<&str> = parts.collect();
    Some(match product_data {
        Some(product_data) => {
            crate::text_commands::analysis::execute_analysis_command_result(product_data, &args)
                .await
        }
        None => Err(crate::text_commands::ManagementCommandError::NotAdmitted(
            "Analysis workspace is unavailable.".to_string(),
        )),
    })
}

#[cfg(feature = "channels")]
async fn channel_papers_response(
    product_data: Option<&crate::product_data_io::ScopedProductData>,
    message: &str,
) -> Option<Result<String, crate::text_commands::ManagementCommandError>> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/papers" {
        return None;
    }
    let args: Vec<&str> = parts.collect();
    Some(match product_data {
        Some(product_data) => {
            crate::text_commands::research::execute_papers_command_result(product_data, &args).await
        }
        None => Err(crate::text_commands::ManagementCommandError::NotAdmitted(
            "Research workspace is unavailable.".to_string(),
        )),
    })
}

#[cfg(feature = "channels")]
struct ChannelManagementResponse {
    message: String,
    terminal: crate::chat_driver::TurnOutcome,
    effect: ControlEffectOutcome,
}

#[cfg(feature = "channels")]
impl ChannelManagementResponse {
    fn from_result(result: Result<String, crate::text_commands::ManagementCommandError>) -> Self {
        use crate::text_commands::ManagementCommandError;
        match result {
            Ok(message) => Self {
                message,
                terminal: crate::chat_driver::TurnOutcome::Completed,
                effect: ControlEffectOutcome::Applied,
            },
            Err(error) => {
                let effect = match &error {
                    ManagementCommandError::InvalidInput(_) => ControlEffectOutcome::Rejected {
                        reason:
                            "Original management request was rejected without a business mutation"
                                .to_string(),
                    },
                    ManagementCommandError::ReadFailed(_) => ControlEffectOutcome::Rejected {
                        reason: "Original read-only management request failed without mutation"
                            .to_string(),
                    },
                    ManagementCommandError::NotAdmitted(_) => ControlEffectOutcome::NotAdmitted {
                        reason:
                            "Original management owner confirmed no business operation admission"
                                .to_string(),
                    },
                    ManagementCommandError::Unknown(_) => ControlEffectOutcome::Unknown {
                        reason: "Original management mutation returned no confirmed effect result"
                            .to_string(),
                    },
                };
                let message = error.to_string();
                Self {
                    terminal: crate::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "management_command",
                            message.clone(),
                        ),
                    ),
                    message,
                    effect,
                }
            }
        }
    }
}

#[cfg(feature = "channels")]
fn channel_attachment_data(
    index: usize,
    attachment: &echo_agent::channels::MessageAttachment,
) -> crate::types::AttachmentData {
    use base64::Engine as _;
    use echo_agent::channels::AttachmentKind;

    let fallback_name = match attachment.kind {
        AttachmentKind::Image => "image.png",
        AttachmentKind::File => "attachment.bin",
        AttachmentKind::Audio => "audio.bin",
        AttachmentKind::Video => "video.bin",
    };
    let name = attachment
        .filename
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}-{fallback_name}", index.saturating_add(1)));
    let inferred = crate::attachments::infer_mime_type(&name);
    let mime_type = if inferred != "application/octet-stream" {
        inferred
    } else {
        match attachment.kind {
            AttachmentKind::Image => "image/png",
            AttachmentKind::File | AttachmentKind::Audio | AttachmentKind::Video => {
                "application/octet-stream"
            }
        }
    };
    crate::types::AttachmentData {
        name,
        mime_type: mime_type.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(&attachment.data),
        size: u64::try_from(attachment.data.len()).unwrap_or(u64::MAX),
        source: crate::types::AttachmentSource::Channel,
    }
}

#[cfg(all(feature = "channels", test))]
fn stage_channel_attachments(
    attachments: &[echo_agent::channels::MessageAttachment],
    execution_root: &Path,
) -> Result<Vec<crate::attachments::AttachmentRef>, String> {
    let mut staged = Vec::with_capacity(attachments.len());
    for (index, attachment) in attachments.iter().enumerate() {
        let data = channel_attachment_data(index, attachment);
        match crate::attachments::stage_attachment_data(&data, Some(execution_root)) {
            Ok(reference) => staged.push(reference),
            Err(error) => {
                let cleanup = crate::attachments::discard_staged_attachment_refs(&staged).err();
                let suffix = cleanup
                    .map(|cleanup| format!("; staged attachment cleanup failed: {cleanup}"))
                    .unwrap_or_default();
                return Err(format!("{error}{suffix}"));
            }
        }
    }
    Ok(staged)
}

#[cfg(feature = "channels")]
fn stage_channel_attachment_data(
    attachments: &[crate::types::AttachmentData],
    execution_root: &Path,
) -> Result<Vec<crate::attachments::AttachmentRef>, String> {
    let mut staged = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        match crate::attachments::stage_attachment_data(attachment, Some(execution_root)) {
            Ok(reference) => staged.push(reference),
            Err(error) => {
                let cleanup = crate::attachments::discard_staged_attachment_refs(&staged).err();
                let suffix = cleanup
                    .map(|cleanup| format!("; staged attachment cleanup failed: {cleanup}"))
                    .unwrap_or_default();
                return Err(format!("{error}{suffix}"));
            }
        }
    }
    Ok(staged)
}

/// Persist channel attachments and construct the canonical turn on the bounded
/// product-data I/O pool. The exact workspace receipt remains owned by the
/// blocking closure, while the foreground supervisor owns this future after
/// admission, so surface cancellation cannot detach a half-written turn.
#[cfg(feature = "channels")]
struct ChannelTurnPreparation {
    attachments: Vec<crate::types::AttachmentData>,
    execution_root: std::path::PathBuf,
    text: String,
    conversation_id: String,
    turn_id: String,
    runtime_authored: bool,
    workspace_io_receipt: crate::state::ScopedWorkspaceIoReceipt,
}

#[cfg(feature = "channels")]
async fn prepare_channel_turn(
    preparation: ChannelTurnPreparation,
    flow: &crate::product_data_io::ProductDataIoFlow,
) -> Result<crate::prepared_turn::PreparedUserTurn, String> {
    flow.run("prepare channel user turn", move || {
        let ChannelTurnPreparation {
            attachments,
            execution_root,
            text,
            conversation_id,
            turn_id,
            runtime_authored,
            workspace_io_receipt,
        } = preparation;
        let _workspace_io_receipt = workspace_io_receipt;
        let attachment_refs = stage_channel_attachment_data(&attachments, &execution_root)?;
        let spill_dir = crate::prepared_turn::resolve_user_input_spill_dir(Some(&execution_root));
        match crate::prepared_turn::PreparedUserTurn::build(crate::prepared_turn::UserTurnInput {
            text: &text,
            attachments: &attachment_refs,
            spill_dir: &spill_dir,
            conversation_id: Some(&conversation_id),
            turn_id: Some(&turn_id),
        }) {
            Ok(turn) if runtime_authored => Ok(turn.runtime_authored()),
            Ok(turn) => Ok(turn),
            Err(error) => {
                let cleanup =
                    crate::attachments::discard_staged_attachment_refs(&attachment_refs).err();
                let suffix = cleanup
                    .map(|cleanup| format!("; staged attachment cleanup failed: {cleanup}"))
                    .unwrap_or_default();
                Err(format!("{error}{suffix}"))
            }
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(feature = "channels")]
const FLUSH_THRESHOLD: usize = 80;

/// 把共享 `ChatDriverEvent` 流投影为有界的内部文本 drafts。
///
/// Native assistant blocks retain their envelope and identity through the
/// channel projection. Legacy Token/Think arms are intentionally absent here;
/// generic FinalAnswer/ExecutionStatus and Terminal paths remain authoritative.
///
/// These sentence/newline boundaries do not directly release transport
/// messages. Dynamic continuation drafts are buffered up to the 252 KiB
/// ordinary-turn cap, canonically redacted once at an atomic/terminal/end
/// boundary, and only then UTF-8 chunked/rate-limited by `outbound`. Overflow
/// fails closed to a fixed omission notice. Agent terminal receipts remain in
/// their separately reserved slot and close the projection last.
///
/// 生命周期:返回流借用 'a(随 `events`),由 `try_stream!` 自然处理(宏生成的
/// future 持有 `events` 的借用)。UTF-8 安全:全用 chars() 判长和拆分
/// (AGENTS.md §1);无 unwrap/expect(§2)。
#[cfg(all(feature = "channels", test))]
async fn aggregate_by_sentence<'a>(
    events: futures::stream::BoxStream<'a, echo_agent::error::Result<ChannelRenderEvent>>,
    channel_id: String,
    to: String,
    chat_type: echo_agent::channels::ChatType,
) -> futures::stream::BoxStream<'a, echo_agent::error::Result<echo_agent::channels::OutboundMessage>>
{
    let repository = Arc::new(
        crate::tool_execution::ToolExecutionRepository::without_initialization(
            std::env::temp_dir().join(format!("eko-channel-render-test-{}", uuid::Uuid::new_v4())),
        ),
    );
    aggregate_by_sentence_with_repository(events, channel_id, to, chat_type, repository, None).await
}

#[cfg(feature = "channels")]
async fn aggregate_by_sentence_with_repository<'a>(
    mut events: futures::stream::BoxStream<'a, echo_agent::error::Result<ChannelRenderEvent>>,
    channel_id: String,
    to: String,
    chat_type: echo_agent::channels::ChatType,
    tool_executions: Arc<crate::tool_execution::ToolExecutionRepository>,
    service_cancel: Option<echo_agent::agent::CancellationToken>,
) -> futures::stream::BoxStream<'a, echo_agent::error::Result<echo_agent::channels::OutboundMessage>>
{
    use crate::chat_driver::ChatDriverEvent;
    use echo_agent::agent::AgentEvent;
    use futures::StreamExt;

    let s = async_stream::try_stream! {
        let mut buf = String::new();
        let mut tool_state = ChannelToolRenderState::default();
        let mut tool_capacity_reported = false;
        let mut tool_identity_conflict_reported = false;
        let mut native_events = HashSet::new();
        let mut unknown_blocks: HashMap<_, String> = HashMap::new();
        let mut unknown_bytes = 0_usize;
        let mut confirmed_executions = HashSet::new();
        // flush 全 buf(若非空)的统一动作,被多个终态/flush 分支共用。
        macro_rules! flush_all {
            () => {
                if !buf.is_empty() {
                    yield ChannelOutboundDraft::stream(std::mem::take(&mut buf));
                }
            };
        }
        macro_rules! discard_unknown_execution {
            ($stream:expr, $turn:expr, $execution:expr) => {
                unknown_blocks.retain(|(stream, turn, execution, _, _), text| {
                    let discard = stream == $stream && turn == $turn && execution == $execution;
                    if discard { unknown_bytes = unknown_bytes.saturating_sub(text.len()); }
                    !discard
                });
            };
        }
        while let Some(ev) = events.next().await {
            let event = match ev? {
                ChannelRenderEvent::Journaled(envelope) => {
                    ChannelRenderEvent::Driver(envelope.payload)
                }
                event => event,
            };
            match event {
                ChannelRenderEvent::ToolProjection(update) => {
                    match tool_state.observe(update) {
                        ChannelToolObserveOutcome::Accepted
                        | ChannelToolObserveOutcome::Duplicate => {}
                        ChannelToolObserveOutcome::Capacity if !tool_capacity_reported => {
                            tool_capacity_reported = true;
                            flush_all!();
                            yield ChannelOutboundDraft::ordinary(
                                "[tool] live detail capacity reached; canonical events remain available in the durable trace.",
                            );
                        }
                        ChannelToolObserveOutcome::IdentityConflict
                            if !tool_identity_conflict_reported =>
                        {
                            tool_identity_conflict_reported = true;
                            flush_all!();
                            yield ChannelOutboundDraft::ordinary(
                                "[tool] canonical identity conflict; channel detail and artifact references were withheld.",
                            );
                        }
                        ChannelToolObserveOutcome::Capacity
                        | ChannelToolObserveOutcome::IdentityConflict => {}
                    }
                }
                ChannelRenderEvent::Prompt(prompt) => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(prompt);
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::Agent(envelope)) => {
                let fallback_message_id = envelope
                    .message_id
                    .as_ref()
                    .map(|message_id| message_id.as_str())
                    .unwrap_or_else(|| envelope.turn_id.as_str())
                    .to_string();
                let chat_owner = ChannelToolOwner::Chat(fallback_message_id.clone());
                match envelope.payload {
                AgentEvent::AssistantMessage { block } => {
                    // 仅去重原event身份的重复呈现，不生成消息或完成事实。
                    let observation = (
                        envelope.stream_id.clone(), envelope.turn_id.clone(),
                        envelope.execution_id.clone(), envelope.event_id,
                    );
                    if !native_events.insert(observation) { continue; }
                    let block_key = (
                        envelope.stream_id, envelope.turn_id, envelope.execution_id,
                        block.request_id, block.block_id,
                    );
                    match block.phase {
                        echo_agent::agent::AssistantPhase::Commentary
                        | echo_agent::agent::AssistantPhase::PartialAnswer => {
                            if block.phase == echo_agent::agent::AssistantPhase::Commentary
                                && let Some(previous) = unknown_blocks.remove(&block_key)
                            {
                                unknown_bytes = unknown_bytes.saturating_sub(previous.len());
                                buf.push_str(&previous);
                            }
                            buf.push_str(&block.delta);
                            if block.complete {
                                flush_all!();
                                // 公开块完成是文字呈现边界，不是执行终态。
                                yield ChannelOutboundDraft::ordinary(String::new());
                            } else if buf.chars().count() >= FLUSH_THRESHOLD {
                                flush_all!();
                            }
                        }
                        // reasoning含签名/opaque原件已原样经过sink和journal；
                        // IM不将其或未知/最终候选转成公开进度。
                        echo_agent::agent::AssistantPhase::Unknown => {
                            if block.delta.len() > outbound::CHANNEL_TURN_EVENT_BYTES.saturating_sub(unknown_bytes) {
                                Err(echo_agent::error::ReactError::Other("channel unknown-message projection exceeds its existing event-byte budget".to_string()))?;
                            }
                            unknown_bytes = unknown_bytes.saturating_add(block.delta.len());
                            unknown_blocks.entry(block_key).or_default().push_str(&block.delta);
                        }
                        echo_agent::agent::AssistantPhase::Reasoning
                        | echo_agent::agent::AssistantPhase::FinalAnswer => {}
                    }
                }
                // 原生状态已由原sink/journal记录；它们不包含用户回答，也不是
                // 手机进度文案。只由真实Terminal发布失败/取消，不在此发送内部ID。
                AgentEvent::ExecutionStatus(_) => {}
                AgentEvent::ToolCall { call_id, invocation } => {
                    flush_all!();
                    let address = tool_state.chat_address(&call_id, &fallback_message_id);
                    let args_preview = channel_tool_args_preview(&invocation.args);
                    let detail = tool_state
                        .detail_ref(&address)
                        .map(|detail_ref| {
                            format!(
                                " [detail {}]",
                                channel_safe_text(detail_ref, CHANNEL_TOOL_PROGRESS_CHARS)
                            )
                        })
                        .unwrap_or_default();
                    yield ChannelOutboundDraft::ordinary(
                        format!(
                            "[tool:{}] started {}: {}{}",
                            channel_safe_text(&call_id, CHANNEL_TOOL_PROGRESS_CHARS),
                            channel_safe_text(&invocation.name, CHANNEL_TOOL_PROGRESS_CHARS),
                            args_preview,
                            detail
                        )
                    );
                }
                AgentEvent::ToolStream {
                    call_id,
                    name,
                    event: echo_agent::tools::ToolStreamEvent::Progress { message, percent },
                } => {
                    let address = tool_state.chat_address(&call_id, &fallback_message_id);
                    if let Some(preview) = tool_state.progress_preview(&address, &message) {
                        flush_all!();
                        let percent = percent
                            .map(|percent| format!(" {percent}%"))
                            .unwrap_or_default();
                        yield ChannelOutboundDraft::ordinary(
                            format!(
                                "[tool:{}] progress {}{}: {}",
                                channel_safe_text(&call_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                channel_safe_text(&name, CHANNEL_TOOL_PROGRESS_CHARS),
                                percent,
                                preview
                            )
                        );
                    }
                }
                AgentEvent::ToolStream {
                    call_id,
                    name,
                    event: echo_agent::tools::ToolStreamEvent::Output { channel, chunk },
                } => {
                    let address = tool_state.chat_address(&call_id, &fallback_message_id);
                    if let Some(preview) = tool_state.output_preview(&address, &chunk) {
                        flush_all!();
                        let channel = match channel {
                            echo_agent::tools::ToolOutputChannel::Stdout => "stdout",
                            echo_agent::tools::ToolOutputChannel::Stderr => "stderr",
                            echo_agent::tools::ToolOutputChannel::Log => "log",
                        };
                        yield ChannelOutboundDraft::ordinary(
                            format!(
                                "[tool:{}] {channel} {}: {}",
                                channel_safe_text(&call_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                channel_safe_text(&name, CHANNEL_TOOL_PROGRESS_CHARS),
                                preview
                            )
                        );
                    }
                }
                AgentEvent::ToolStream {
                    call_id,
                    name,
                    event: echo_agent::tools::ToolStreamEvent::Complete(result),
                }
                | AgentEvent::ToolResult {
                    call_id,
                    name,
                    result,
                } => {
                    flush_all!();
                    let address = tool_state.chat_address(&call_id, &fallback_message_id);
                    match tool_state.finish(&address) {
                        ChannelToolTerminal::Duplicate => {}
                        ChannelToolTerminal::IdentityConflict => {
                            yield ChannelOutboundDraft::ordinary(format!(
                                "[tool:{}] result withheld because canonical tool identity conflicted; inspect the durable trace.",
                                channel_safe_text(&call_id, CHANNEL_TOOL_PROGRESS_CHARS),
                            ));
                        }
                        ChannelToolTerminal::Render(entry) => {
                            let artifact = channel_verified_artifact(
                                Arc::clone(&tool_executions),
                                entry.as_deref().map(|entry| &entry.summary),
                                &result,
                            )
                            .await;
                            yield ChannelOutboundDraft::ordinary(channel_tool_result_message(
                                entry.as_deref(),
                                artifact.as_ref(),
                                &call_id,
                                &name,
                                &result,
                            ));
                        }
                    }
                }
                AgentEvent::FinalAnswer(answer) => {
                    flush_all!();
                    if answer.trim().is_empty() {
                        tool_state.finish_owner(&chat_owner);
                        continue;
                    }
                    discard_unknown_execution!(&envelope.stream_id, &envelope.turn_id, &envelope.execution_id);
                    {
                        // FinalAnswer是原Framework校验/持久化后的正文发布，不证明后继物理结算成功。
                        // 按原事件顺序呈现；Terminal仍独占foreground关闭，
                        // 不由此把Agent执行结束提升成TaskRun完成。
                        let execution = (
                            envelope.stream_id,
                            envelope.turn_id,
                            envelope.execution_id,
                        );
                        if confirmed_executions.insert(execution) {
                            yield ChannelOutboundDraft::confirmed_answer(answer);
                        }
                    }
                    tool_state.finish_owner(&chat_owner);
                }
                AgentEvent::Cancelled => {
                    flush_all!();
                    discard_unknown_execution!(&envelope.stream_id, &envelope.turn_id, &envelope.execution_id);
                    tool_state.finish_owner(&chat_owner);
                }
                AgentEvent::Error { .. } => {
                    flush_all!();
                    discard_unknown_execution!(&envelope.stream_id, &envelope.turn_id, &envelope.execution_id);
                    tool_state.finish_owner(&chat_owner);
                }
                AgentEvent::BudgetDecision { decision, reason, .. } => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[budget] {decision:?}: {reason}"));
                }
                AgentEvent::GuardTriggered { guard, blocked } => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[guard] {guard} (blocked={blocked})"));
                }
                AgentEvent::MemoryRecalled { count } => {
                    tracing::debug!(count, "channel agent recalled memory");
                }
                AgentEvent::Chart { spec } => {
                    flush_all!();
                    let preview: String = spec.to_string().chars().take(500).collect();
                    yield ChannelOutboundDraft::ordinary(format!("[chart] {preview}"));
                }
                AgentEvent::SafetyNotice { action, reason, risk, permission } => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[safety] {action}: {reason} (risk={risk}, permission={permission})"));
                }
                AgentEvent::ParameterError { tool, parameter, expected, got } => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[parameter] {tool}.{parameter}: expected {expected}, got {got}"));
                }
                _ => {}
                }
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::Execution(event)) => {
                    let mut handled_tool_event = false;
                    if event.scope
                        == crate::tasks::task_runtime::executor::ExecEventScope::Subagent
                        && let Some(subagent_run_id) = event.subagent_run_id.as_deref()
                    {
                        let owner = ChannelToolOwner::Subagent(subagent_run_id.to_string());
                        match event.event {
                            crate::tasks::task_runtime::RuntimeEventKind::ToolStarted => {
                                handled_tool_event = true;
                                if let Ok(payload) = serde_json::from_value::<ChannelExecutionToolStarted>(event.payload.clone()) {
                                    flush_all!();
                                    let address = ChannelToolAddress::subagent(
                                        &event.workspace_id,
                                        &event.conversation_id,
                                        &event.run_id,
                                        subagent_run_id,
                                        &payload.call_id,
                                    );
                                    let detail = tool_state.detail_ref(&address).map(|detail_ref| {
                                        format!(" [detail {}]", channel_safe_text(detail_ref, CHANNEL_TOOL_PROGRESS_CHARS))
                                    }).unwrap_or_default();
                                    yield ChannelOutboundDraft::ordinary(format!(
                                        "[subagent:{} tool:{}] started {}: {}{}",
                                        channel_safe_text(subagent_run_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                        channel_safe_text(&payload.call_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                        channel_safe_text(&payload.invocation.name, CHANNEL_TOOL_PROGRESS_CHARS),
                                        channel_tool_args_preview(&payload.invocation.args),
                                        detail,
                                    ));
                                }
                            }
                            crate::tasks::task_runtime::RuntimeEventKind::ToolOutput => {
                                handled_tool_event = true;
                                if let Ok(payload) = serde_json::from_value::<ChannelExecutionToolOutput>(event.payload.clone()) {
                                    let address = ChannelToolAddress::subagent(
                                        &event.workspace_id,
                                        &event.conversation_id,
                                        &event.run_id,
                                        subagent_run_id,
                                        &payload.call_id,
                                    );
                                    let projected = match (payload.channel, payload.chunk, payload.message) {
                                        (Some(channel), Some(chunk), None) => tool_state
                                            .output_preview(&address, &chunk)
                                            .map(|preview| format!(
                                                "[subagent:{} tool:{}] {} {}: {}",
                                                channel_safe_text(subagent_run_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                                channel_safe_text(&payload.call_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                                channel_safe_text(&channel, CHANNEL_TOOL_PROGRESS_CHARS),
                                                channel_safe_text(&payload.name, CHANNEL_TOOL_PROGRESS_CHARS),
                                                preview,
                                            )),
                                        (None, None, Some(message)) => tool_state
                                            .progress_preview(&address, &message)
                                            .map(|preview| {
                                                let percent = payload.percent.map(|percent| format!(" {percent}%")).unwrap_or_default();
                                                format!(
                                                    "[subagent:{} tool:{}] progress {}{}: {}",
                                                    channel_safe_text(subagent_run_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                                    channel_safe_text(&payload.call_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                                    channel_safe_text(&payload.name, CHANNEL_TOOL_PROGRESS_CHARS),
                                                    percent,
                                                    preview,
                                                )
                                            }),
                                        _ => None,
                                    };
                                    if let Some(projected) = projected {
                                        flush_all!();
                                        yield ChannelOutboundDraft::ordinary(projected);
                                    }
                                }
                            }
                            crate::tasks::task_runtime::RuntimeEventKind::ToolCompleted => {
                                handled_tool_event = true;
                                if let Ok(payload) = serde_json::from_value::<ChannelExecutionToolCompleted>(event.payload.clone()) {
                                    flush_all!();
                                    let address = ChannelToolAddress::subagent(
                                        &event.workspace_id,
                                        &event.conversation_id,
                                        &event.run_id,
                                        subagent_run_id,
                                        &payload.call_id,
                                    );
                                    match tool_state.finish(&address) {
                                        ChannelToolTerminal::Duplicate => {}
                                        ChannelToolTerminal::IdentityConflict => {
                                            yield ChannelOutboundDraft::ordinary(format!(
                                                "[subagent:{} tool:{}] result withheld because canonical tool identity conflicted; inspect the durable trace.",
                                                channel_safe_text(subagent_run_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                                channel_safe_text(&payload.call_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                            ));
                                        }
                                        ChannelToolTerminal::Render(entry) => {
                                            let artifact = channel_verified_artifact(
                                                Arc::clone(&tool_executions),
                                                entry.as_deref().map(|entry| &entry.summary),
                                                &payload.result,
                                            ).await;
                                            yield ChannelOutboundDraft::ordinary(format!(
                                                "[subagent:{}] {}",
                                                channel_safe_text(subagent_run_id, CHANNEL_TOOL_PROGRESS_CHARS),
                                                channel_tool_result_message(
                                                    entry.as_deref(),
                                                    artifact.as_ref(),
                                                    &payload.call_id,
                                                    &payload.name,
                                                    &payload.result,
                                                )
                                            ));
                                        }
                                    }
                                }
                            }
                            crate::tasks::task_runtime::RuntimeEventKind::Cancelled
                            | crate::tasks::task_runtime::RuntimeEventKind::TimedOut
                            | crate::tasks::task_runtime::RuntimeEventKind::Completed
                            | crate::tasks::task_runtime::RuntimeEventKind::Failed => {
                                tool_state.finish_owner(&owner);
                            }
                            _ => {}
                        }
                    }
                    if !handled_tool_event && event.event.is_attention_event() {
                        flush_all!();
                        let detail: String = event.payload.to_string().chars().take(500).collect();
                        yield ChannelOutboundDraft::ordinary(format!("[task:{}] {}: {detail}", event.run_id, event.event));
                    }
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::TurnStatus { .. })
                | ChannelRenderEvent::Driver(ChatDriverEvent::ExecutionPath { .. })
                | ChannelRenderEvent::Driver(ChatDriverEvent::TurnConfiguration { .. }) => {}
                ChannelRenderEvent::Driver(ChatDriverEvent::Interrupt { run_id, goal, new_message }) => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[paused:{run_id}] {goal}; new instruction: {new_message}"));
                }
                // 输入持久化/排队/ACK事实继续留在原journal；不主动向用户
                // 投递input ID或逐阶段调试标签。公开提示与HITL仍走原分支。
                ChannelRenderEvent::Driver(ChatDriverEvent::InputLifecycle(_)) => {}
                ChannelRenderEvent::Driver(ChatDriverEvent::CommandCellStarted { cell }) => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[cell:{}] started: {}", cell.cell_id, cell.name));
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::CommandCellSettled { cell }) => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[cell:{}] settled: {}", cell.cell_id, cell.phase));
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::CommandCellWatchReady { result }) => {
                    flush_all!();
                    let event = ChatDriverEvent::CommandCellWatchReady { result };
                    let message = crate::tasks::task_runtime::project_command_cell_watch_surface_event(&event)
                        .map(|projection| projection.display_message())
                        .unwrap_or_else(|| "CommandCellWatch result is unavailable".to_string());
                    yield ChannelOutboundDraft::ordinary(message);
                }
                ChannelRenderEvent::Driver(
                    ChatDriverEvent::CommandCellWatchDeliveryStarted { .. }
                    | ChatDriverEvent::CommandCellWatchAcknowledged { .. },
                ) => {}
                ChannelRenderEvent::Driver(ChatDriverEvent::ExtensionReceipt(receipt)) => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(receipt.display_message());
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::ApprovalRequest {
                    request_id,
                    tool_name,
                    prompt,
                    ..
                }) => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[approval:{request_id}] {tool_name}: {prompt}"));
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::InputRequest { request_id, prompt, .. }) => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[input:{request_id}] {prompt}"));
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::SelectionRequest {
                    request_id,
                    prompt,
                    options,
                    ..
                }) => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[selection:{request_id}] {prompt} ({})", options.join(", ")));
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::HitlRequestReleased { .. })
                | ChannelRenderEvent::Driver(ChatDriverEvent::WeixinControlDelivery(_)) => {}
                ChannelRenderEvent::Driver(ChatDriverEvent::ContextCompressed {
                    before_count,
                    after_count,
                    before_tokens,
                    after_tokens,
                }) => {
                    flush_all!();
                    let saved = before_tokens.saturating_sub(after_tokens);
                    yield ChannelOutboundDraft::ordinary(
                        format!(
                            "[context] compressed {before_count}->{after_count} messages, \
                             {before_tokens}->{after_tokens} tokens ({saved} saved)"
                        )
                    );
                }
                ChannelRenderEvent::Terminal(
                    crate::chat_driver::TurnOutcome::Completed,
                ) => {
                    flush_all!();
                    break;
                }
                ChannelRenderEvent::Terminal(
                    crate::chat_driver::TurnOutcome::Cancelled,
                ) => {
                    flush_all!();
                    yield ChannelOutboundDraft::terminal("本次处理已取消。");
                    break;
                }
                ChannelRenderEvent::Terminal(
                    crate::chat_driver::TurnOutcome::Failed(failure),
                ) => {
                    flush_all!();
                    yield ChannelOutboundDraft::terminal(format!("本次处理失败：{}", failure.message));
                    break;
                }
                ChannelRenderEvent::Journaled(_) => {}
            }
        }
    };
    channel_outbound_transport(s.boxed(), channel_id, to, chat_type, service_cancel)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "channels")]
    use super::{
        ChannelHumanLoopProvider, ChannelHumanLoopResolution, ChannelRenderEvent,
        drive_with_channel_prompts,
    };
    #[cfg(feature = "channels")]
    use std::sync::Arc;
    #[cfg(feature = "channels")]
    struct SessionIdentityProbe;

    #[cfg(feature = "channels")]
    fn native_commentary_block(
        text: impl Into<String>,
        sequence: u64,
    ) -> echo_agent::agent::AgentEvent {
        echo_agent::agent::AgentEvent::AssistantMessage {
            block: echo_agent::agent::AssistantMessageBlock {
                provider_source: None,
                block_id: format!("native-consumer-block-{sequence}"),
                request_id: "native-consumer-request".to_string(),
                sequence,
                phase: echo_agent::agent::AssistantPhase::Commentary,
                delta: text.into(),
                complete: true,
                provider: Some("native-consumer-fixture".to_string()),
                reasoning: None,
            },
        }
    }

    #[cfg(feature = "channels")]
    fn native_commentary_event(
        sequence: u64,
        text: impl Into<String>,
    ) -> echo_agent::error::Result<crate::chat_driver::ChatDriverEvent> {
        let identity = echo_agent::agent::EventIdentity::new(
            "native-consumer-stream",
            "native-consumer-turn",
        )?;
        Ok(crate::chat_driver::ChatDriverEvent::Agent(Box::new(
            echo_agent::agent::EventEnvelope::new(
                &identity,
                sequence,
                None,
                native_commentary_block(text, sequence),
            )?,
        )))
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn completed_service_work_is_idle_and_preserves_original_failure() -> Result<(), String> {
        let coordinator = super::ChannelSessionCoordinator::new();
        let (completed, observed) = tokio::sync::oneshot::channel();
        coordinator
            .input_pump_tasks
            .lock()
            .map_err(|_| "service owner unavailable")?
            .tasks
            .lock()
            .map_err(|_| "service JoinSet unavailable")?
            .spawn(async move {
                let _ = completed.send(());
                Err("original service failure".to_string())
            });
        observed.await.map_err(|error| error.to_string())?;
        // 空闲观察收割已完成的原任务，无需等待调用者尚未发起的 shutdown。
        assert!(!coordinator.has_pending_service_work()?);
        let receipt = coordinator.observe_input_pump_settlement().await?;
        assert_eq!(receipt.failures, vec!["original service failure"]);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn recovery_claims_have_one_owner_per_transport_epoch() -> Result<(), String> {
        let claims = std::sync::Mutex::new(super::ChannelRecoveryClaims::default());
        let identity = crate::conversation_input::ConversationInputIdentity {
            address: crate::conversation_input::ConversationInputAddress {
                workspace_id: "global".to_string(),
                conversation_id: "conversation".to_string(),
            },
            input_id: "input".to_string(),
            revision: 1,
            payload_sha256: "payload".to_string(),
        };
        assert!(super::ChannelSessionCoordinator::claim_recovery_identity(
            &claims, 7, &identity,
        )?);
        assert!(!super::ChannelSessionCoordinator::claim_recovery_identity(
            &claims, 7, &identity,
        )?);
        assert!(super::ChannelSessionCoordinator::claim_recovery_identity(
            &claims, 8, &identity,
        )?);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[async_trait::async_trait]
    impl echo_agent::channels::MessageHandler for SessionIdentityProbe {
        async fn handle(
            &self,
            message: echo_agent::channels::InboundMessage,
        ) -> echo_agent::error::Result<echo_agent::channels::OutboundMessage> {
            Ok(echo_agent::channels::OutboundMessage::new(
                &message.channel_id,
                message.reply_target(),
                message.chat_type,
                "ok",
            ))
        }

        async fn reply(
            &self,
            _message: echo_agent::channels::OutboundMessage,
        ) -> echo_agent::error::Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "channels")]
    struct ChannelTestSink;

    #[cfg(feature = "channels")]
    impl crate::chat_driver::ChatSink for ChannelTestSink {
        fn on_event(&self, _event: crate::chat_driver::ChatDriverEvent) -> bool {
            true
        }
    }

    #[cfg(feature = "channels")]
    struct SharedChannelHandler(Arc<super::AppChannelMessageHandler>);

    #[cfg(feature = "channels")]
    #[async_trait::async_trait]
    impl echo_agent::channels::MessageHandler for SharedChannelHandler {
        async fn handle(
            &self,
            msg: echo_agent::channels::InboundMessage,
        ) -> echo_agent::error::Result<echo_agent::channels::OutboundMessage> {
            echo_agent::channels::MessageHandler::handle(self.0.as_ref(), msg).await
        }
        async fn reply(
            &self,
            msg: echo_agent::channels::OutboundMessage,
        ) -> echo_agent::error::Result<()> {
            echo_agent::channels::MessageHandler::reply(self.0.as_ref(), msg).await
        }
        fn has_pending_work(&self) -> bool {
            echo_agent::channels::MessageHandler::has_pending_work(self.0.as_ref())
        }
        fn bind_durable_ingress(
            &self,
            cancel: echo_agent::agent::CancellationToken,
        ) -> echo_agent::error::Result<()> {
            echo_agent::channels::MessageHandler::bind_durable_ingress(self.0.as_ref(), cancel)
        }
        async fn close(&self) -> echo_agent::error::Result<()> {
            echo_agent::channels::MessageHandler::close(self.0.as_ref()).await
        }
        async fn admit_with_ingress(
            &self,
            msg: echo_agent::channels::InboundMessage,
            cancel: echo_agent::agent::CancellationToken,
            receipt: echo_agent::channels::ChannelIngressReceipt,
        ) -> echo_agent::error::Result<()> {
            echo_agent::channels::MessageHandler::admit_with_ingress(
                self.0.as_ref(),
                msg,
                cancel,
                receipt,
            )
            .await
        }
    }

    #[cfg(feature = "channels")]
    async fn seed_absent_weixin_context(
        store: &dyn echo_agent::memory::Store,
        generation: &str,
        message_id: &str,
    ) -> anyhow::Result<()> {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        use sha2::{Digest, Sha256};
        let account = URL_SAFE_NO_PAD.encode("account");
        let generation = URL_SAFE_NO_PAD.encode(generation);
        let key = format!(
            "message-context:{:x}",
            Sha256::digest(serde_json::to_vec(&("peer", message_id))?)
        );
        store
            .put(
                &["weixin-protocol-v1", &account, &generation],
                &key,
                serde_json::json!({"peer":"peer", "message_id":message_id, "context":null}),
            )
            .await?;
        Ok(())
    }

    #[cfg(feature = "channels")]
    struct DurableAdmissionSink;
    #[cfg(feature = "channels")]
    impl crate::chat_driver::ChatSink for DurableAdmissionSink {
        fn on_event(&self, _event: crate::chat_driver::ChatDriverEvent) -> bool {
            true
        }
    }

    #[cfg(feature = "channels")]
    struct DurableAdmissionRouteFixture {
        _directory: tempfile::TempDir,
        _resource_scope: Arc<crate::runtime::ApplicationLifecycleRetention>,
        state: Arc<crate::state::AppState>,
        store: Arc<crate::tasks::task_runtime::TaskRuntimeStore>,
        llm: Arc<ChannelCancellationBarrierLlmClient>,
        agent: echo_agent::agent::AgentHandle,
        foreground: crate::foreground_turn::ForegroundTurnControl,
        sessions: echo_agent::channels::SessionHandler,
        coordinator: Arc<super::ChannelSessionCoordinator>,
        root: tokio::task::JoinHandle<
            Result<crate::chat_driver::TurnOutcome, crate::chat_driver::ChatTurnError>,
        >,
        cancel: echo_agent::agent::CancellationToken,
        conversation: String,
        root_turn: String,
        run_id: String,
        target: String,
        protocol_store: Arc<echo_agent::memory::InMemoryStore>,
        _channel: echo_agent::channels::weixin::WeixinChannel,
    }

    #[cfg(feature = "channels")]
    impl DurableAdmissionRouteFixture {
        async fn new(
            surface: crate::foreground_turn::ForegroundTurnSurface,
        ) -> anyhow::Result<Self> {
            use echo_agent::channels::{
                ChatType, InboundMessage, MessageHandler, SessionConfig, SessionHandler,
                channel_ingress_receipt,
            };
            let directory = tempfile::tempdir()?;
            let llm = Arc::new(ChannelCancellationBarrierLlmClient::new());
            let primary = echo_agent::agent::AgentHandle::new(
                echo_agent::agent::ReactAgentBuilder::new()
                    .model("durable-admission-root")
                    .llm_client(llm.clone())
                    .build()?,
            );
            let pool = Arc::new(
                crate::agent_pool::AgentPool::new_for_test(primary.clone(), None, None, 4, false)
                    .await,
            );
            pool.set_llm_client_override_for_test(llm.clone()).await;
            let store = Arc::new(
                crate::tasks::task_runtime::TaskRuntimeStore::open_for_workspace(
                    directory.path().join("tasks"),
                    "global",
                )?,
            );
            let mut state = crate::state::AppState::from_shared(
                primary,
                None,
                Arc::new(crate::hitl::HitlDispatcher::new()),
                None,
                None,
                Default::default(),
                Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                    directory.path().join("mcp.json"),
                    Default::default(),
                )),
                crate::product_data_io::ProductDataIoService::new(),
            )?;
            state.tasks.runtime = Some(store.clone());
            state.storage.chat_events = Arc::new(crate::chat_event_log::ChatEventLog::open(
                directory.path().join("chat"),
                Default::default(),
            )?);
            state.workspace.global_execution_root = directory.path().join("product-data");
            std::fs::create_dir_all(&state.workspace.global_execution_root)?;
            state.set_pool(pool.clone());
            let resource_scope =
                crate::runtime::ApplicationLifecycleRetention::resource_scope_for_test();
            store.bind_lifecycle_retention(&resource_scope);
            pool.bind_lifecycle_retention(&resource_scope);
            state
                .workspace
                .runtimes
                .bind_lifecycle_retention(&resource_scope);
            state
                .session
                .foreground_turns
                .bind_lifecycle_retention(&resource_scope);
            let state = Arc::new(state);
            let runtime = state.current_control_runtime().await?;
            let foreground = state.session.foreground_turns.clone();
            let coordinator = Arc::new(super::ChannelSessionCoordinator::new());
            coordinator.bind_lifecycle_retention(Arc::downgrade(&resource_scope));
            let original_service = echo_agent::agent::CancellationToken::new();
            *coordinator
                .ingress_service_cancel
                .lock()
                .map_err(|_| anyhow::anyhow!("原service binding不可用"))? =
                Some(original_service.clone());
            let protocol_store = Arc::new(echo_agent::memory::InMemoryStore::new());
            let target = echo_agent::channels::weixin::encode_target("account", "peer")?;
            let channel = echo_agent::channels::weixin::WeixinChannel::new(
                echo_agent::channels::weixin::WeixinConfig {
                    credentials: echo_agent::channels::weixin::WeixinCredentials {
                        account_id: "account".into(),
                        bot_token: "offline-fixture".into(),
                        base_url: "https://example.invalid".into(),
                        user_id: "peer".into(),
                        generation: "durable-admission-generation".into(),
                    },
                    store: protocol_store.clone(),
                },
            )?;
            let delivery = channel
                .delivery_handle()
                .ok_or_else(|| anyhow::anyhow!("原Weixin producer handle缺失"))?;
            let captured = Arc::new(std::sync::Mutex::new(None));
            let factory_capture = captured.clone();
            let factory_state = state.clone();
            let factory_foreground = foreground.clone();
            let factory_coordinator = coordinator.clone();
            let factory_runtime = runtime.clone();
            let sessions = SessionHandler::new(
                SessionConfig::default().with_reset_commands(Vec::new()),
                move |instance: &echo_agent::channels::ChannelSessionInstance| {
                    let handler = Arc::new(
                        super::AppChannelMessageHandler::new(
                            factory_state.clone(),
                            Arc::new(crate::webhook::WebhookEmitter::new()),
                            factory_foreground.clone(),
                            instance.clone(),
                            factory_coordinator.clone(),
                        )
                        .with_bound_control_runtime(factory_runtime.clone())
                        .with_weixin_delivery(delivery.clone())
                        .with_weixin_credential_identity("account", "durable-admission-generation"),
                    );
                    *factory_capture
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(handler.clone());
                    Box::new(SharedChannelHandler(handler)) as Box<dyn MessageHandler>
                },
            )
            .with_durable_ingress();
            sessions.bind_durable_ingress(original_service)?;
            let (publisher, receipt) = channel_ingress_receipt();
            sessions
                .admit_with_ingress(
                    InboundMessage::new(
                        "weixin",
                        &target,
                        &target,
                        ChatType::Direct,
                        "",
                        "bootstrap-empty",
                    ),
                    echo_agent::agent::CancellationToken::new(),
                    publisher,
                )
                .await?;
            let bootstrap = tokio::time::timeout(std::time::Duration::from_secs(2), receipt)
                .await
                .map_err(|error| {
                    anyhow::anyhow!("durable fixture original bootstrap receipt deadline: {error}")
                })??;
            if bootstrap != echo_agent::channels::ChannelIngressDisposition::TerminalRejected {
                sessions.close().await?;
                return Err(anyhow::anyhow!("空初始化消息没有按真实入口拒绝"));
            }
            let handler = captured
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .ok_or_else(|| anyhow::anyhow!("原SessionHandler未构造应用handler"))?;
            let conversation =
                super::AppChannelMessageHandler::conversation_id("weixin", &target, &target);
            let agent_conversation = super::AppChannelMessageHandler::agent_conversation_id(
                &conversation,
                &handler.session_instance.incarnation_id(),
            );
            let execution = pool
                .acquire(&agent_conversation)
                .await
                .map_err(anyhow::Error::msg)?;
            let agent = execution.agent();
            let root_turn = "durable-admission-original-root".to_string();
            let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&root_turn);
            let lease = runtime
                .begin_turn(&foreground, surface, &conversation, root_turn.clone())
                .await?;
            let cancel = lease.cancellation_token();
            let resources = Arc::new(crate::chat_resources::ChatResources {
                execution_scope: runtime.execution_scope().clone(),
                workspace_io_receipt: Some(runtime.workspace_io_receipt()),
                pool: Some(pool),
                store: Some(store.clone()),
                sink: Arc::new(DurableAdmissionSink),
                webhook_emitter: None,
                conv_id: Some(conversation.clone()),
                root_message_id: root_turn.clone(),
                attachments: Vec::new(),
                cancel: cancel.clone(),
                review_integration: None,
                memory_generation: None,
                human_loop_provider: None,
            });
            let root_agent = agent.clone();
            let root = tokio::spawn(async move {
                let held = Arc::new(std::sync::Mutex::new(Some(execution)));
                lease.retain_execution(held.clone());
                let turn = crate::prepared_turn::PreparedUserTurn {
                    instruction: "原根轮次仅等取消".into(),
                    resources: Vec::new(),
                    authorship: crate::prepared_turn::InstructionAuthorship::User,
                    agent_input: None,
                };
                crate::chat_driver::with_foreground_pool_execution(
                    Arc::downgrade(&held),
                    crate::foreground_turn::drive_foreground_chat(
                        lease,
                        &root_agent,
                        &turn,
                        resources,
                    ),
                )
                .await
            });
            if let Err(error) =
                tokio::time::timeout(std::time::Duration::from_secs(3), llm.wait_started()).await
            {
                let source_started = llm.started.load(std::sync::atomic::Ordering::Acquire);
                let model_calls = llm.calls.load(std::sync::atomic::Ordering::Acquire);
                let driver_finished = root.is_finished();
                let foreground_present = foreground
                    .snapshot_scoped(
                        runtime.execution_scope().workspace_id(),
                        surface,
                        &conversation,
                    )
                    .is_some();
                let run_active = store.is_run_active(&run_id);
                cancel.cancel();
                let original_outcome = root.await;
                let original_close = sessions.close().await;
                return Err(anyhow::anyhow!(
                    "durable fixture original model-start deadline: {error}; started={source_started}; calls={model_calls}; driver_finished={driver_finished}; foreground_present={foreground_present}; run_active={run_active}; root={original_outcome:?}; close={original_close:?}"
                ));
            }
            if surface == crate::foreground_turn::ForegroundTurnSurface::Channel {
                let surface_id =
                    super::ChannelSessionCoordinator::surface_id(&handler.session_instance);
                if let Err(error) = handler.publish_active_turn(
                    &surface_id,
                    &conversation,
                    &agent_conversation,
                    &runtime,
                    &root_turn,
                ) {
                    cancel.cancel();
                    let _ = root.await;
                    let _ = sessions.close().await;
                    return Err(anyhow::anyhow!(error));
                }
            }
            Ok(Self {
                _directory: directory,
                _resource_scope: resource_scope,
                state,
                store,
                llm,
                agent,
                foreground,
                sessions,
                coordinator,
                root,
                cancel,
                conversation,
                root_turn,
                run_id,
                target,
                protocol_store,
                _channel: channel,
            })
        }

        async fn close(self) -> anyhow::Result<()> {
            use echo_agent::channels::MessageHandler;
            self.cancel.cancel();
            let root = self.root.await;
            let sessions = self.sessions.close().await;
            let admission = self.coordinator.begin_input_pump_shutdown();
            let pumps = self.coordinator.observe_input_pump_settlement().await;
            root?.map_err(|error| anyhow::anyhow!(error.to_string()))?;
            sessions?;
            admission.map_err(anyhow::Error::msg)?;
            pumps.map_err(anyhow::Error::msg)?;
            Ok(())
        }
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn actual_gui_shared_consumer_barrier_fault_never_calls_mailbox_and_retains_original_target()
    -> anyhow::Result<()> {
        use crate::conversation_input::{
            ConversationInputAddress, ConversationInputPhase, ConversationInputSource,
        };
        use crate::tasks::task_runtime::RuntimeEventKind;
        let fixture =
            DurableAdmissionRouteFixture::new(crate::foreground_turn::ForegroundTurnSurface::Gui)
                .await?;
        let result = async {
            let service = fixture.state.conversation_inputs();
            let address = ConversationInputAddress {
                workspace_id: "global".into(),
                conversation_id: fixture.conversation.clone(),
            };
            let saved = service
                .submit(
                    address.clone(),
                    "gui-barrier-input".into(),
                    "原GUI追加输入不得丢失🧭".into(),
                    Vec::new(),
                )
                .await?;
            let started = service
                .dispatch_selected(
                    saved.identity,
                    saved.queue_revision,
                    fixture.root_turn.clone(),
                )
                .await?;
            let attempt = started
                .active_attempt
                .clone()
                .ok_or_else(|| anyhow::anyhow!("原GUI attempt缺失"))?;
            fixture
                .store
                .fail_next_input_acceptance_durability_for_test(&fixture.run_id)?;
            let delivered = fixture
                .state
                .steer_user_input_for_active_turn(
                    &fixture.foreground,
                    ConversationInputSource::Gui,
                    &started,
                    &fixture.agent,
                    echo_agent::prelude::Message::user("原GUI追加输入不得丢失🧭".to_string()),
                )
                .await;
            let rejected = delivered.is_err();
            let observed = service
                .observe_durable_steer_through_drain(attempt.clone(), delivered)
                .await?;
            let again = fixture
                .state
                .steer_user_input_for_active_turn(
                    &fixture.foreground,
                    ConversationInputSource::Gui,
                    &started,
                    &fixture.agent,
                    echo_agent::prelude::Message::user("原GUI追加输入不得丢失🧭".to_string()),
                )
                .await;
            let frontier = service.list(&address).await?;
            let item = frontier
                .items
                .first()
                .ok_or_else(|| anyhow::anyhow!("原GUI输入丢失"))?;
            Ok::<_, anyhow::Error>((
                rejected,
                again.is_err(),
                observed.phase,
                item.payload.text.clone(),
                item.task_target.clone(),
                attempt.observation.mailbox_invocations(),
                fixture.llm.calls.load(std::sync::atomic::Ordering::Acquire),
                fixture
                    .store
                    .list_events(&fixture.run_id, 0)?
                    .iter()
                    .filter(|event| event.event_type == RuntimeEventKind::RunInputAccepted)
                    .count(),
            ))
        }
        .await;
        let expected_run = fixture.run_id.clone();
        let cleanup = fixture.close().await;
        let (rejected, retry_rejected, phase, text, target, mailbox, models, accepted) = result?;
        cleanup?;
        assert!(rejected && retry_rejected);
        assert_eq!(phase, ConversationInputPhase::RecoveryRequired);
        assert_eq!(text, "原GUI追加输入不得丢失🧭");
        assert_eq!(
            target.as_ref().map(|target| target.run_id.as_str()),
            Some(expected_run.as_str())
        );
        assert_eq!(mailbox, 0);
        assert_eq!(models, 1);
        assert_eq!(accepted, 1);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn actual_session_handler_barrier_fault_retains_input_without_new_model_or_mailbox_acceptance()
    -> anyhow::Result<()> {
        use crate::conversation_input::{ConversationInputAddress, ConversationInputPhase};
        use crate::tasks::task_runtime::RuntimeEventKind;
        use echo_agent::channels::{
            ChatType, InboundMessage, MessageHandler, channel_ingress_receipt,
        };
        let fixture = DurableAdmissionRouteFixture::new(
            crate::foreground_turn::ForegroundTurnSurface::Channel,
        )
        .await?;
        let result=async {
            fixture.store.fail_next_input_acceptance_durability_for_test(&fixture.run_id)?;
            seed_absent_weixin_context(fixture.protocol_store.as_ref(),"durable-admission-generation","im-barrier-input").await?;
            let message=InboundMessage::new("weixin",&fixture.target,&fixture.target,ChatType::Direct,"/steer 真实IM追加输入🧭","im-barrier-input");
            let (publisher,receipt)=channel_ingress_receipt();
            fixture.sessions.admit_with_ingress(message.clone(),echo_agent::agent::CancellationToken::new(),publisher).await?;
            let source_receipt=tokio::time::timeout(std::time::Duration::from_secs(2),receipt).await??;
            let address=ConversationInputAddress {workspace_id:"global".into(),conversation_id:fixture.conversation.clone()};
            let item=tokio::time::timeout(std::time::Duration::from_secs(3),async {
                loop {
                    let frontier=fixture.state.conversation_inputs().list(&address).await?;
                    if let Some(item)=frontier.items.into_iter().find(|item|item.receipt.phase==ConversationInputPhase::RecoveryRequired) {
                        break Ok::<_,crate::conversation_input::ConversationInputError>(item);
                    }
                    tokio::task::yield_now().await;
                }
            }).await??;
            let (duplicate,receipt)=channel_ingress_receipt();
            fixture.sessions.admit_with_ingress(message,echo_agent::agent::CancellationToken::new(),duplicate).await?;
            let duplicate_source=tokio::time::timeout(std::time::Duration::from_secs(2),receipt).await??;
            let log=fixture.state.storage.chat_events.replay("global",Some(&fixture.conversation),&fixture.root_turn,0)?;
            let mailbox=log.events.iter().filter(|event|matches!(&event.payload,crate::chat_driver::ChatDriverEvent::InputLifecycle(fact)
                if matches!(fact.as_ref(),crate::conversation_input::ConversationInputFact::MailboxAccepted{..}))).count();
            let physical_mailbox=item.active_attempt.as_ref().ok_or_else(||anyhow::anyhow!("原IM attempt观察缺失"))?.observation.mailbox_invocations();
            Ok::<_,anyhow::Error>((item.payload.text,item.task_target,mailbox,physical_mailbox,source_receipt,duplicate_source,
                fixture.llm.calls.load(std::sync::atomic::Ordering::Acquire),
                fixture.store.list_events(&fixture.run_id,0)?.iter().filter(|event|event.event_type==RuntimeEventKind::RunInputAccepted).count()))
        }.await;
        let expected_run = fixture.run_id.clone();
        let cleanup = fixture.close().await;
        let (
            text,
            target,
            mailbox,
            physical_mailbox,
            source_receipt,
            duplicate_source,
            models,
            accepted,
        ) = result?;
        cleanup?;
        assert_eq!(
            source_receipt,
            echo_agent::channels::ChannelIngressDisposition::DurableInput
        );
        assert_eq!(
            duplicate_source,
            echo_agent::channels::ChannelIngressDisposition::DurableInput
        );
        assert_eq!(physical_mailbox, 0);
        assert_eq!(text, "/steer 真实IM追加输入🧭");
        assert_eq!(
            target.as_ref().map(|target| target.run_id.as_str()),
            Some(expected_run.as_str())
        );
        assert_eq!(mailbox, 0);
        assert_eq!(models, 1);
        assert_eq!(accepted, 1);
        Ok(())
    }

    #[cfg(feature = "channels")]
    struct WeixinEffectFixture {
        _directory: tempfile::TempDir,
        _sessions: echo_agent::channels::SessionHandler,
        state: Arc<crate::state::AppState>,
        runtime: crate::state::ScopedChatRuntime,
        foreground: crate::foreground_turn::ForegroundTurnControl,
        coordinator: Arc<super::ChannelSessionCoordinator>,
        target: String,
        conversation: String,
    }

    #[cfg(feature = "channels")]
    impl WeixinEffectFixture {
        async fn new() -> anyhow::Result<Self> {
            Self::from_directory(tempfile::tempdir()?).await
        }

        async fn from_directory(directory: tempfile::TempDir) -> anyhow::Result<Self> {
            Self::with_agent(
                directory,
                channel_test_agent(std::time::Duration::ZERO).map_err(anyhow::Error::msg)?,
                false,
                None,
            )
            .await
        }

        async fn with_agent(
            directory: tempfile::TempDir,
            agent: echo_agent::agent::AgentHandle,
            reflection: bool,
            browser: Option<Arc<crate::browser::BrowserRuntime>>,
        ) -> anyhow::Result<Self> {
            use echo_agent::channels::{
                ChatType, InboundMessage, MessageHandler, SessionConfig, SessionHandler,
            };
            let mut state = crate::state::AppState::from_shared(
                agent,
                None,
                Arc::new(crate::hitl::HitlDispatcher::new()),
                None,
                None,
                Default::default(),
                Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                    directory.path().join("mcp.json"),
                    echo_agent::mcp::McpConfigFile::default(),
                )),
                crate::product_data_io::ProductDataIoService::new(),
            )?;
            state.storage.chat_events = Arc::new(crate::chat_event_log::ChatEventLog::open(
                directory.path().join("chat-events"),
                crate::chat_event_log::ChatEventRetention::default(),
            )?);
            // Every management command in this fixture uses a private data
            // root; native file effects never touch checkout/user artifacts.
            state.workspace.global_execution_root = directory.path().join("product-data");
            std::fs::create_dir_all(&state.workspace.global_execution_root)?;
            state.history.workflows = Arc::new(crate::workflow_service::WorkflowService::open(
                directory.path().join("workflows.json"),
            )?);
            if browser.is_some() {
                // 浏览器原 Extension 路径要求同一 global pool 和 plugin runtime；
                // 只装 BrowserRuntime 会在确认请求前被 scoped_context 拒绝。
                let agent = state.connection.primary_agent();
                let pool = Arc::new(
                    crate::agent_pool::AgentPool::new_for_test(agent.clone(), None, None, 4, false)
                        .await,
                );
                let plugin_runtime = crate::plugin_runtime::PluginRuntimeService::new_for_test(
                    agent,
                    state.workspace.global_execution_root.clone(),
                    directory.path().join("browser-extension-plugins.json"),
                    directory.path().join("browser-extension-plugin-data"),
                )
                .await?;
                state.set_pool(pool);
                state = state.with_plugin_runtime(Some(plugin_runtime));
            }
            state = state.with_browser_runtime(browser);
            if reflection {
                state = state.with_review_integration(Some(Arc::new(
                    crate::evolution::ReviewIntegration::new_scoped(
                        echo_agent::evolution::ReviewConfig::default(),
                        directory.path().join("review"),
                        Arc::new(echo_agent::memory::InMemoryStore::new()),
                        "global".into(),
                        "global".into(),
                    ),
                )));
            }
            let state = Arc::new(state);
            let factory_state = state.clone();
            let runtime = state.current_control_runtime().await?;
            let captured = Arc::new(std::sync::Mutex::new(None));
            let captured_for_factory = captured.clone();
            let foreground = crate::foreground_turn::ForegroundTurnControl::default();
            let foreground_for_factory = foreground.clone();
            let bound = runtime.clone();
            let coordinator = Arc::new(super::ChannelSessionCoordinator::new());
            let original_service = echo_agent::agent::CancellationToken::new();
            *coordinator
                .ingress_service_cancel
                .lock()
                .map_err(|_| anyhow::anyhow!("service binding unavailable"))? =
                Some(original_service.clone());
            let factory_coordinator = coordinator.clone();
            let sessions = SessionHandler::new(
                SessionConfig::default().with_reset_commands(Vec::new()),
                move |instance: &echo_agent::channels::ChannelSessionInstance| {
                    let handler = Arc::new(
                        super::AppChannelMessageHandler::new(
                            factory_state.clone(),
                            Arc::new(crate::webhook::WebhookEmitter::new()),
                            foreground_for_factory.clone(),
                            instance.clone(),
                            factory_coordinator.clone(),
                        )
                        .with_bound_control_runtime(bound.clone())
                        .with_weixin_credential_identity("account", "effect-generation"),
                    );
                    *captured_for_factory
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(handler.clone());
                    Box::new(SharedChannelHandler(handler)) as Box<dyn MessageHandler>
                },
            )
            .with_durable_ingress();
            sessions.bind_durable_ingress(original_service)?;
            let target = echo_agent::channels::weixin::encode_target("account", "peer")?;
            {
                let (publisher, receipt) = echo_agent::channels::channel_ingress_receipt();
                sessions
                    .admit_with_ingress(
                        InboundMessage::new(
                            "weixin",
                            &target,
                            &target,
                            ChatType::Direct,
                            "",
                            "fixture",
                        ),
                        echo_agent::agent::CancellationToken::new(),
                        publisher,
                    )
                    .await?;
                assert_eq!(
                    receipt.await?,
                    echo_agent::channels::ChannelIngressDisposition::TerminalRejected
                );
            }
            let _handler = captured
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .ok_or_else(|| anyhow::anyhow!("real handler missing"))?;
            let conversation =
                super::AppChannelMessageHandler::conversation_id("weixin", &target, &target);
            Ok(Self {
                _directory: directory,
                _sessions: sessions,
                state,
                runtime,
                foreground,
                coordinator,
                target,
                conversation,
            })
        }

        async fn reopen(self) -> anyhow::Result<Self> {
            use echo_agent::channels::MessageHandler;
            self._sessions.close().await?;
            self.coordinator
                .begin_input_pump_shutdown()
                .map_err(anyhow::Error::msg)?;
            self.coordinator
                .observe_input_pump_settlement()
                .await
                .map_err(anyhow::Error::msg)?;
            let Self {
                _directory,
                _sessions,
                state,
                runtime,
                foreground,
                coordinator,
                target: _,
                conversation: _,
            } = self;
            drop(_sessions);
            drop(state);
            drop(runtime);
            drop(foreground);
            drop(coordinator);
            Self::from_directory(_directory).await
        }

        async fn control(
            &self,
            text: &str,
            provider_id: &str,
        ) -> anyhow::Result<Option<echo_agent::channels::ChannelIngressDisposition>> {
            let receipt = self.admit(text, provider_id).await?;
            // Diagnostic text and EOF are deliberately ignored. Only the
            // original effect owner's typed receipt is the test oracle.
            Ok(
                tokio::time::timeout(std::time::Duration::from_secs(2), receipt)
                    .await?
                    .ok(),
            )
        }

        async fn admit(
            &self,
            text: &str,
            provider_id: &str,
        ) -> anyhow::Result<
            tokio::sync::oneshot::Receiver<echo_agent::channels::ChannelIngressDisposition>,
        > {
            use echo_agent::channels::{
                ChatType, InboundMessage, MessageHandler, channel_ingress_receipt,
            };
            let (publisher, receipt) = channel_ingress_receipt();
            self._sessions
                .admit_with_ingress(
                    InboundMessage::new(
                        "weixin",
                        &self.target,
                        &self.target,
                        ChatType::Direct,
                        text,
                        provider_id,
                    ),
                    echo_agent::agent::CancellationToken::new(),
                    publisher,
                )
                .await?;
            Ok(receipt)
        }

        async fn close(&self) -> anyhow::Result<()> {
            use echo_agent::channels::MessageHandler;
            self._sessions.close().await?;
            self.coordinator
                .begin_input_pump_shutdown()
                .map_err(anyhow::Error::msg)?;
            self.coordinator
                .observe_input_pump_settlement()
                .await
                .map_err(anyhow::Error::msg)?;
            self.foreground.shutdown().await?;
            Ok(())
        }
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn durable_reflect_admits_before_llm_and_close_joins_original_effect()
    -> anyhow::Result<()> {
        use echo_agent::channels::ChannelIngressDisposition;
        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_response("A durable reflection")
                .with_delay(std::time::Duration::from_secs(3)),
        );
        let agent = echo_agent::agent::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("native-reflection")
                .llm_client(llm.clone())
                .build()?,
        );
        let fixture = Arc::new(
            WeixinEffectFixture::with_agent(tempfile::tempdir()?, agent, true, None).await?,
        );
        let mut receipt = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            fixture.admit("/reflect", "owned-reflection"),
        )
        .await??;
        assert!(matches!(
            receipt.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while llm.call_count() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert!(
            fixture
                .coordinator
                .has_pending_service_work()
                .map_err(anyhow::Error::msg)?
        );
        let closer = fixture.clone();
        let first_close = tokio::spawn(async move { closer.close().await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let started = fixture
                    .coordinator
                    .input_pump_tasks
                    .lock()
                    .map_err(|_| anyhow::anyhow!("service settlement unavailable"))?
                    .settlement
                    .is_some();
                if started {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        // Drop an actual close observer after the original producer is owned.
        // The retry joins that SAME producer and native memory write.
        first_close.abort();
        assert!(matches!(first_close.await, Err(error) if error.is_cancelled()));
        assert!(
            fixture
                .coordinator
                .has_pending_service_work()
                .map_err(anyhow::Error::msg)?
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), fixture.close()).await??;
        assert_eq!(receipt.await?, ChannelIngressDisposition::ControlHandled);
        assert_eq!(llm.call_count(), 1);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn durable_workflow_run_returns_before_original_graph_result() -> anyhow::Result<()> {
        use echo_agent::channels::ChannelIngressDisposition;
        let fixture = WeixinEffectFixture::new().await?;
        let workflow = fixture.state.history.workflows.create("owned-router".into(),
            "name: owned-router\nnodes:\n  - name: route\n    type: router\nedges: []\nentry: route\nfinish: [route]\n".into())?;
        let command = format!("/workflow run {} {{\"original_input\":true}}", workflow.id);
        let mut receipt = fixture.admit(&command, "owned-workflow").await?;
        // Current-thread execution has returned from the real SessionHandler;
        // its newly owned native operation has not been polled by this task.
        assert!(matches!(
            receipt.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(
            fixture
                .coordinator
                .has_pending_service_work()
                .map_err(anyhow::Error::msg)?
        );
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), receipt).await??,
            ChannelIngressDisposition::ControlHandled
        );
        assert_eq!(
            fixture.control(&command, "owned-workflow").await?,
            Some(ChannelIngressDisposition::ControlHandled)
        );
        fixture.close().await?;
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn durable_browser_confirmation_releases_admit_and_preserves_native_policy()
    -> anyhow::Result<()> {
        use echo_agent::channels::ChannelIngressDisposition;
        let directory = tempfile::tempdir()?;
        let browser = crate::browser::BrowserRuntime::start(crate::browser::BrowserConfig {
            enabled: false,
            extension_enabled: false,
            session_dir: directory.path().join("browser-sessions"),
            ..Default::default()
        })
        .await;
        let approval = Arc::new(crate::hitl::ChannelHumanLoopProvider::new());
        let mut prompts = approval.subscribe_prompts();
        browser
            .set_default_approval_provider(approval.clone())
            .await;
        let fixture = WeixinEffectFixture::with_agent(
            directory,
            channel_test_agent(std::time::Duration::ZERO).map_err(anyhow::Error::msg)?,
            false,
            Some(browser.clone()),
        )
        .await?;
        let command = "/browser click-target {\"target\":\"pay\",\"double_click\":false,\"effect\":\"purchase\"}";
        let mut receipt = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            fixture.admit(command, "owned-browser-confirmation"),
        )
        .await??;
        tokio::time::timeout(std::time::Duration::from_secs(2), prompts.recv()).await??;
        assert!(matches!(
            receipt.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(
            fixture
                .foreground
                .snapshot_scoped(
                    fixture.runtime.execution_scope().workspace_id(),
                    crate::foreground_turn::ForegroundTurnSurface::Channel,
                    &fixture.conversation
                )
                .is_some()
        );
        // Decline at the existing approval provider; no browser sidecar/Node or
        // HTTP action is started. A failed specialist receipt stays conservative.
        approval.close_now("Test declines the original consequential action");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), receipt).await??,
            ChannelIngressDisposition::ControlRecoveryRequired
        );
        fixture.close().await?;
        browser.shutdown().await;
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn reset_and_compact_busy_are_not_handled_and_same_provider_id_can_retry()
    -> anyhow::Result<()> {
        for command in ["/reset", "/compact"] {
            let fixture = WeixinEffectFixture::new().await?;
            // The original channel inspector excludes GUI turns, but actual
            // foreground admission is cross-surface. This deterministically reaches
            // the REAL reset/compact Busy branches without cancellation first.
            let gui = fixture.foreground.begin_scoped(
                fixture.runtime.execution_scope().workspace_id(),
                crate::foreground_turn::ForegroundTurnSurface::Gui,
                &fixture.conversation,
                "gui-still-running",
            )?;
            let provider_id = format!("busy:{command}");
            assert!(fixture.control(command, &provider_id).await?.is_none());
            // If the first diagnostic/EOF were incorrectly Handled or Unknown,
            // replay would produce Handled/RecoveryRequired instead of another
            // proven no-admission deferral of this exact ID.
            assert!(fixture.control(command, &provider_id).await?.is_none());
            assert!(!gui.cancellation_token().is_cancelled());
            gui.settle_after_observers(crate::chat_driver::TurnOutcome::Completed)
                .await?;
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn reset_cancel_observation_is_recovery_required_and_never_handled() -> anyhow::Result<()>
    {
        use echo_agent::channels::ChannelIngressDisposition;
        let fixture = WeixinEffectFixture::new().await?;
        let lease = fixture.foreground.begin_scoped(
            fixture.runtime.execution_scope().workspace_id(),
            crate::foreground_turn::ForegroundTurnSurface::Channel,
            &fixture.conversation,
            "original-not-settled",
        )?;
        lease.retain_observation(crate::chat_driver::ChatTurnError::Observation {
            run_id: "original-run".to_string(),
            execution_id: Some("physical-execution".to_string()),
        });
        assert_eq!(
            fixture.control("/reset", "reset-after-observation").await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        assert_eq!(
            fixture.control("/reset", "reset-after-observation").await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        assert!(lease.cancellation_token().is_cancelled());
        assert!(
            fixture
                .foreground
                .snapshot_scoped(
                    fixture.runtime.execution_scope().workspace_id(),
                    crate::foreground_turn::ForegroundTurnSurface::Channel,
                    &fixture.conversation
                )
                .is_some()
        );
        assert!(matches!(
            lease
                .settle_after_observers(crate::chat_driver::TurnOutcome::Cancelled)
                .await,
            Err(crate::foreground_turn::ForegroundTurnError::DriverSettlement(_))
        ));
        assert!(
            fixture
                .foreground
                .snapshot_scoped(
                    fixture.runtime.execution_scope().workspace_id(),
                    crate::foreground_turn::ForegroundTurnSurface::Channel,
                    &fixture.conversation,
                )
                .is_some()
        );
        let reopened = fixture.reopen().await?;
        assert_eq!(
            reopened
                .control("/reset", "reset-after-observation")
                .await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn successful_control_replays_original_result_after_real_session_reopen()
    -> anyhow::Result<()> {
        use echo_agent::channels::ChannelIngressDisposition;
        let fixture = WeixinEffectFixture::new().await?;
        assert_eq!(
            fixture
                .control("/tools list", "original-read-success")
                .await?,
            Some(ChannelIngressDisposition::ControlHandled)
        );
        let reopened = fixture.reopen().await?;
        assert_eq!(
            reopened
                .control("/tools list", "original-read-success")
                .await?,
            Some(ChannelIngressDisposition::ControlHandled)
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn developer_registry_error_is_not_a_successful_control_eof() -> anyhow::Result<()> {
        use echo_agent::channels::ChannelIngressDisposition;
        let fixture = WeixinEffectFixture::new().await?;
        // The actual registry rejects this action without creating a terminal.
        // Its flat error contract still cannot certify no admission generally.
        assert_eq!(
            fixture
                .control("/terminal invalid-fixture-action", "developer-error")
                .await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        assert_eq!(
            fixture
                .control("/terminal invalid-fixture-action", "developer-error")
                .await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn real_session_flat_control_failures_keep_typed_receipts_on_replay() -> anyhow::Result<()>
    {
        use echo_agent::channels::ChannelIngressDisposition;
        let fixture = WeixinEffectFixture::new().await?;
        for (command, message_id) in [
            ("/tools enable", "tool-usage"),
            ("/workflow show", "workflow-usage"),
            ("/agent-send missing", "agent-send-usage"),
            ("/agent-group create incomplete", "agent-group-usage"),
            ("/task-goal", "task-goal-usage"),
        ] {
            assert_eq!(
                fixture.control(command, message_id).await?,
                Some(ChannelIngressDisposition::TerminalRejected)
            );
            assert_eq!(
                fixture.control(command, message_id).await?,
                Some(ChannelIngressDisposition::TerminalRejected)
            );
        }
        // The native setter error may follow policy publication, so its generic
        // error contract is not a safe-retry proof. Do not inspect error text.
        assert_eq!(
            fixture
                .control(
                    "/tools enable fixture-unregistered-tool",
                    "tool-native-error"
                )
                .await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        assert_eq!(
            fixture
                .control(
                    "/tools enable fixture-unregistered-tool",
                    "tool-native-error"
                )
                .await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        // Missing TaskRuntime is proven pre-admission and returns Retryable;
        // the original Result closes normally without an effect ACK/EOF wait.
        assert!(
            fixture
                .control("/task-status", "task-unavailable")
                .await?
                .is_none()
        );
        assert!(
            fixture
                .control("/task-status", "task-unavailable")
                .await?
                .is_none()
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn real_session_management_rejections_and_original_owner_refusal_never_complete()
    -> anyhow::Result<()> {
        use echo_agent::channels::ChannelIngressDisposition;
        let fixture = WeixinEffectFixture::new().await?;
        for (text, id) in [
            ("/analysis invalid", "management-invalid"),
            ("/analysis create unsupported title", "management-language"),
            ("/analysis save id invalid-json", "management-json"),
            ("/papers delete-source", "management-papers-usage"),
            (
                "/analysis show missing-original",
                "management-analysis-read",
            ),
            ("/papers show missing-original", "management-papers-read"),
        ] {
            assert_eq!(
                fixture.control(text, id).await?,
                Some(ChannelIngressDisposition::TerminalRejected)
            );
        }
        let fixture = fixture.reopen().await?;
        for (text, id) in [
            ("/analysis invalid", "management-invalid"),
            ("/analysis create unsupported title", "management-language"),
            ("/analysis save id invalid-json", "management-json"),
            ("/papers delete-source", "management-papers-usage"),
            (
                "/analysis show missing-original",
                "management-analysis-read",
            ),
            ("/papers show missing-original", "management-papers-read"),
        ] {
            assert_eq!(
                fixture.control(text, id).await?,
                Some(ChannelIngressDisposition::TerminalRejected)
            );
        }
        fixture
            .runtime
            .primary_agent()
            .write_async(|agent| {
                Box::pin(async move {
                    agent.run_store = None;
                })
            })
            .await;
        assert!(
            fixture
                .control("/trace original-run", "trace-store-unavailable")
                .await?
                .is_none()
        );
        assert!(
            fixture
                .control("/trace original-run", "trace-store-unavailable")
                .await?
                .is_none()
        );
        // The real supervisor refuses before publishing a run. No Python/R
        // process or synthetic command executor is introduced by this fixture.
        fixture.state.session.analysis_runs.begin_shutdown();
        assert!(
            fixture
                .control("/analysis run original-analysis", "analysis-start-closed")
                .await?
                .is_none()
        );
        assert!(
            fixture
                .control("/analysis run original-analysis", "analysis-start-closed")
                .await?
                .is_none()
        );
        // Original ProductData I/O admission refuses before the closure runs.
        fixture
            .state
            .session
            .product_data_io
            .begin_shutdown()
            .map_err(anyhow::Error::msg)?;
        assert!(
            fixture
                .control(
                    "/papers add-source refused before write",
                    "papers-io-closed"
                )
                .await?
                .is_none()
        );
        assert!(
            fixture
                .control(
                    "/papers add-source refused before write",
                    "papers-io-closed"
                )
                .await?
                .is_none()
        );
        let fixture = fixture.reopen().await?;
        assert_eq!(
            fixture
                .control(
                    "/papers add-source refused before write",
                    "papers-io-closed"
                )
                .await?,
            Some(ChannelIngressDisposition::ControlHandled)
        );
        assert_eq!(
            crate::research::list_sources(fixture.runtime.execution_scope().root(), None, None)?
                .len(),
            1
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn real_session_management_native_errors_and_success_survive_original_journal_reopen()
    -> anyhow::Result<()> {
        use echo_agent::channels::ChannelIngressDisposition;
        let fixture = WeixinEffectFixture::new().await?;
        let create = "/papers add-source original controlled paper";
        assert_eq!(
            fixture.control(create, "papers-real-create").await?,
            Some(ChannelIngressDisposition::ControlHandled)
        );
        let fixture = fixture.reopen().await?;
        assert_eq!(
            fixture.control(create, "papers-real-create").await?,
            Some(ChannelIngressDisposition::ControlHandled)
        );
        let root = fixture.runtime.execution_scope().root().to_path_buf();
        let sources = crate::research::list_sources(&root, None, None)?;
        assert_eq!(
            sources.len(),
            1,
            "completed provider replay must not execute create twice"
        );
        let source = sources
            .first()
            .ok_or_else(|| anyhow::anyhow!("actual created source missing"))?;
        let source_path = root
            .join("research")
            .join("sources")
            .join(format!("{}.json", source.id));
        let original_bytes = std::fs::read(&source_path)?;
        std::fs::remove_file(&source_path)?;
        let delete = format!("/papers delete-source {}", source.id);
        // Reach delete_source's actual native Err, not a mock/usage renderer.
        assert_eq!(
            fixture.control(&delete, "papers-real-error").await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        // Restore the target as an observable witness: replaying the original
        // mutation would delete it. The same unknown identity must not do so.
        std::fs::write(&source_path, &original_bytes)?;
        let analysis_root = root.join("analysis");
        std::fs::write(&analysis_root, "original native I/O refusal")?;
        assert_eq!(
            fixture
                .control(
                    "/analysis create python real failure",
                    "analysis-real-io-error"
                )
                .await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        let fixture = fixture.reopen().await?;
        assert_eq!(
            fixture.control(&delete, "papers-real-error").await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        assert_eq!(
            std::fs::read(&source_path)?,
            original_bytes,
            "unknown provider replay must not run delete again"
        );
        assert_eq!(
            fixture
                .control(
                    "/analysis create python real failure",
                    "analysis-real-io-error"
                )
                .await?,
            Some(ChannelIngressDisposition::ControlRecoveryRequired)
        );
        assert_eq!(
            std::fs::read_to_string(&analysis_root)?,
            "original native I/O refusal"
        );
        assert_eq!(crate::research::list_sources(&root, None, None)?.len(), 1);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn reset_partial_effect_and_failed_settlement_cannot_report_applied() {
        assert!(matches!(
            super::reset_pre_effect_outcome(false, "busy"),
            super::ControlEffectOutcome::NotAdmitted { .. }
        ));
        assert!(matches!(
            super::reset_pre_effect_outcome(true, "busy after cancel"),
            super::ControlEffectOutcome::Unknown { .. }
        ));
        assert!(matches!(
            super::settled_control_outcome(true, false, "failed physical settlement"),
            super::ControlEffectOutcome::Unknown { .. }
        ));
        assert!(matches!(
            super::settled_control_outcome(false, true, "failed operation"),
            super::ControlEffectOutcome::Unknown { .. }
        ));
        assert!(matches!(
            super::settled_control_outcome(true, true, "completed"),
            super::ControlEffectOutcome::Applied
        ));
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn twenty_four_weixin_steers_return_before_the_original_drain() -> Result<(), String> {
        use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let foreground = ForegroundTurnControl::default();
        let lease = foreground
            .begin_scoped(
                "weixin-steer-workspace",
                ForegroundTurnSurface::Channel,
                "conversation",
                "original-root",
            )
            .map_err(|error| error.to_string())?;
        let observed = Arc::new(AtomicUsize::new(0));
        let mut releases = Vec::new();
        for ordinal in 0..24_usize {
            let (steer_tx, steer_rx) = tokio::sync::oneshot::channel();
            let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
            releases.push(drain_tx);
            let observed = observed.clone();
            // This is the same original supervisor used by the real live input
            // path. Its drain remains unavailable (as while phone HITL waits).
            foreground
                .supervise_input_lifecycle_scoped(
                    "weixin-steer-workspace",
                    ForegroundTurnSurface::Channel,
                    "conversation",
                    "original-root",
                    async move {
                        let received = steer_rx.await.map_err(|error| error.to_string())?;
                        if received != ordinal {
                            return Err("original handoff identity changed".to_string());
                        }
                        drain_rx.await.map_err(|error| error.to_string())?;
                        observed.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                    Arc::new(|_| Box::pin(async { Ok(()) })),
                )
                .map_err(|error| error.to_string())?;
            assert!(matches!(
                super::hand_weixin_steer_to_original_owner(steer_tx, ordinal)?,
                super::ChannelLiveInputRoute::WeixinAcceptedPending
            ));
        }
        assert_eq!(observed.load(Ordering::SeqCst), 0);
        for release in releases {
            release
                .send(())
                .map_err(|_| "original drain receiver was dropped".to_string())?;
        }
        lease
            .settle_after_observers(crate::chat_driver::TurnOutcome::Completed)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(observed.load(Ordering::SeqCst), 24);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn saved_binding_reopens_into_real_session_and_stale_generation_starts_no_model()
    -> anyhow::Result<()> {
        use crate::conversation_input::{
            ChannelExecutionIntent, ChannelInputBinding, ConversationInputPayload,
            FrozenChannelContext,
        };
        use echo_agent::channels::weixin::{
            WeixinChannel, WeixinConfig, WeixinCredentials, encode_target,
        };
        use echo_agent::channels::{
            ChannelIngressDisposition, ChatType, InboundMessage, MessageHandler, SessionConfig,
            SessionHandler, channel_ingress_receipt,
        };
        for (workspace_generation, should_start) in [("global", true), ("stale-generation", false)]
        {
            let temporary = tempfile::tempdir()?;
            let target = encode_target("account", "peer")?;
            let conversation =
                super::AppChannelMessageHandler::conversation_id("weixin", &target, &target);
            let address = super::channel_input_address("global", &conversation);
            let input_id = crate::conversation_input::stable_scoped_input_id(
                &address,
                crate::conversation_input::ConversationInputSource::Channel,
                "saved-provider",
            )?;
            let original_identity = {
                let log = Arc::new(crate::chat_event_log::ChatEventLog::open(
                    temporary.path().join("events"),
                    Default::default(),
                )?);
                let payload = ConversationInputPayload::new_for_channel(
                    &address,
                    "saved original input".into(),
                    Vec::new(),
                    None,
                    ChannelInputBinding {
                        version: 1,
                        channel_id: "weixin".into(),
                        account_id: "account".into(),
                        credential_generation: "generation".into(),
                        peer_id: "peer".into(),
                        provider_message_id: "saved-provider".into(),
                        workspace_generation: workspace_generation.into(),
                        context: FrozenChannelContext::Absent,
                        execution: ChannelExecutionIntent::Chat,
                    },
                )?;
                log.submit_conversation_input(address.clone(), input_id, payload)
                    .await?
                    .identity
            };
            // Reopen the real App journal with fresh runtime/session ownership.
            // Only LLM output is held; the original submit/claim/pool path runs.
            let llm = Arc::new(ChannelCancellationBarrierLlmClient::new());
            let primary = echo_agent::agent::AgentHandle::new(
                echo_agent::agent::ReactAgentBuilder::new()
                    .model("channel-cancellation-barrier")
                    .llm_client(llm.clone())
                    .build()?,
            );
            let pool = Arc::new(
                crate::agent_pool::AgentPool::new_for_test(primary.clone(), None, None, 4, false)
                    .await,
            );
            pool.set_llm_client_override_for_test(llm.clone()).await;
            let mut state = crate::state::AppState::from_shared(
                primary,
                None,
                Arc::new(crate::hitl::HitlDispatcher::new()),
                None,
                None,
                Default::default(),
                Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                    temporary.path().join("mcp.json"),
                    Default::default(),
                )),
                crate::product_data_io::ProductDataIoService::new(),
            )?;
            let resource_scope =
                crate::runtime::ApplicationLifecycleRetention::resource_scope_for_test();
            state.tasks.runtime = Some(Arc::new(
                crate::tasks::task_runtime::TaskRuntimeStore::open_for_workspace(
                    temporary.path().join("tasks"),
                    "global",
                )?,
            ));
            state.workspace.global_execution_root = temporary.path().join("product-data");
            std::fs::create_dir_all(&state.workspace.global_execution_root)?;
            state
                .tasks
                .runtime
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("fixture store missing"))?
                .bind_lifecycle_retention(&resource_scope);
            pool.bind_lifecycle_retention(&resource_scope);
            state
                .workspace
                .runtimes
                .bind_lifecycle_retention(&resource_scope);
            state
                .session
                .foreground_turns
                .bind_lifecycle_retention(&resource_scope);
            state.set_pool(pool);
            state.storage.chat_events = Arc::new(crate::chat_event_log::ChatEventLog::open(
                temporary.path().join("events"),
                Default::default(),
            )?);
            let state = Arc::new(state);
            let runtime = state.current_control_runtime().await?;
            let protocol_store = Arc::new(echo_agent::memory::InMemoryStore::new());
            seed_absent_weixin_context(protocol_store.as_ref(), "generation", "saved-provider")
                .await?;
            let channel = WeixinChannel::new(WeixinConfig {
                credentials: WeixinCredentials {
                    account_id: "account".into(),
                    bot_token: "offline-fixture".into(),
                    base_url: "https://example.invalid".into(),
                    user_id: "peer".into(),
                    generation: "generation".into(),
                },
                store: protocol_store,
            })?;
            let delivery = channel
                .delivery_handle()
                .ok_or_else(|| anyhow::anyhow!("original sender unavailable"))?;
            let coordinator = Arc::new(super::ChannelSessionCoordinator::new());
            coordinator.bind_lifecycle_retention(Arc::downgrade(&resource_scope));
            let factory_coordinator = coordinator.clone();
            let factory_state = state.clone();
            let sessions = SessionHandler::new(
                SessionConfig::default().with_reset_commands(Vec::new()),
                move |instance: &echo_agent::channels::ChannelSessionInstance| {
                    Box::new(
                        super::AppChannelMessageHandler::new(
                            factory_state.clone(),
                            Arc::new(crate::webhook::WebhookEmitter::new()),
                            factory_state.session.foreground_turns.clone(),
                            instance.clone(),
                            factory_coordinator.clone(),
                        )
                        .with_bound_control_runtime(runtime.clone())
                        .with_weixin_delivery(delivery.clone())
                        .with_weixin_credential_identity("account", "generation"),
                    ) as Box<dyn MessageHandler>
                },
            )
            .with_durable_ingress();
            let service = echo_agent::agent::CancellationToken::new();
            sessions.bind_durable_ingress(service.child_token())?;
            let message = InboundMessage::new(
                "weixin",
                &target,
                &target,
                ChatType::Direct,
                "saved original input",
                "saved-provider",
            );
            let (receipt, observed) = channel_ingress_receipt();
            let admitted = sessions
                .admit_with_ingress(message.clone(), service.child_token(), receipt)
                .await;
            if should_start {
                admitted?;
                assert_eq!(observed.await?, ChannelIngressDisposition::DurableInput);
                tokio::time::timeout(std::time::Duration::from_secs(5), llm.wait_started()).await?;
                let (again, observed_again) = channel_ingress_receipt();
                sessions
                    .admit_with_ingress(message, service.child_token(), again)
                    .await?;
                assert_eq!(
                    observed_again.await?,
                    ChannelIngressDisposition::DurableInput
                );
                let (inputs, complete) = state.storage.chat_events.channel_input_bindings().await?;
                assert!(complete);
                let original = inputs
                    .iter()
                    .find(|input| input.receipt.identity == original_identity)
                    .ok_or_else(|| anyhow::anyhow!("saved identity lost"))?;
                assert_eq!(original.receipt.attempt, Some(1));
                assert_eq!(llm.calls.load(std::sync::atomic::Ordering::Acquire), 1);
            } else {
                assert!(admitted.is_err());
                assert!(observed.await.is_err());
                assert_eq!(llm.calls.load(std::sync::atomic::Ordering::Acquire), 0);
                let original = state
                    .storage
                    .chat_events
                    .prove_unclaimed_channel_input(original_identity)
                    .await?;
                assert_eq!(original.receipt.attempt, None);
            }
            service.cancel();
            sessions.close().await?;
            coordinator
                .begin_input_pump_shutdown()
                .map_err(anyhow::Error::msg)?;
            let settlement = coordinator
                .observe_input_pump_settlement()
                .await
                .map_err(anyhow::Error::msg)?;
            assert!(settlement.failures.is_empty());
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn real_session_twenty_four_steers_keep_phone_reply_admissible() -> anyhow::Result<()> {
        use echo_agent::channels::weixin::{
            WeixinChannel, WeixinConfig, WeixinCredentials, encode_target,
        };
        use echo_agent::channels::{
            ChannelIngressDisposition, ChatType, InboundMessage, MessageHandler, SessionConfig,
            SessionHandler, channel_ingress_receipt,
        };
        use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
        let temporary = tempfile::tempdir()?;
        let llm = Arc::new(ChannelCancellationBarrierLlmClient::after_intent_router());
        let primary = echo_agent::agent::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("channel-cancellation-barrier")
                .llm_client(llm.clone())
                .build()?,
        );
        let pool = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(primary.clone(), None, None, 4, false).await,
        );
        pool.set_llm_client_override_for_test(llm.clone()).await;
        let mut state = crate::state::AppState::from_shared(
            primary,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                temporary.path().join("mcp.json"),
                echo_agent::mcp::McpConfigFile::default(),
            )),
            crate::product_data_io::ProductDataIoService::new(),
        )?;
        let resource_scope =
            crate::runtime::ApplicationLifecycleRetention::resource_scope_for_test();
        state.tasks.runtime = Some(Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::open_for_workspace(
                temporary.path().join("tasks"),
                "global",
            )?,
        ));
        state.workspace.global_execution_root = temporary.path().join("product-data");
        std::fs::create_dir_all(&state.workspace.global_execution_root)?;
        state
            .tasks
            .runtime
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("fixture store missing"))?
            .bind_lifecycle_retention(&resource_scope);
        pool.bind_lifecycle_retention(&resource_scope);
        state
            .workspace
            .runtimes
            .bind_lifecycle_retention(&resource_scope);
        state
            .session
            .foreground_turns
            .bind_lifecycle_retention(&resource_scope);
        state.set_pool(pool);
        state.storage.chat_events = Arc::new(crate::chat_event_log::ChatEventLog::open(
            temporary.path().join("chat-events"),
            crate::chat_event_log::ChatEventRetention::default(),
        )?);
        let state = Arc::new(state);
        let runtime = state.current_control_runtime().await?;
        let bound = runtime.clone();
        let protocol_store = Arc::new(echo_agent::memory::InMemoryStore::new());
        let channel = WeixinChannel::new(WeixinConfig {
            credentials: WeixinCredentials {
                account_id: "account".to_string(),
                bot_token: "offline-fixture".to_string(),
                base_url: "https://example.invalid".to_string(),
                user_id: "peer".to_string(),
                generation: "generation".to_string(),
            },
            store: protocol_store.clone(),
        })?;
        let delivery = channel
            .delivery_handle()
            .ok_or_else(|| anyhow::anyhow!("original delivery handle missing"))?;
        let foreground = crate::foreground_turn::ForegroundTurnControl::default();
        foreground.bind_lifecycle_retention(&resource_scope);
        let coordinator = Arc::new(super::ChannelSessionCoordinator::new());
        coordinator.bind_lifecycle_retention(Arc::downgrade(&resource_scope));
        let captured = Arc::new(std::sync::Mutex::new(None));
        let factory_captured = captured.clone();
        let factory_state = state.clone();
        let factory_foreground = foreground.clone();
        let factory_coordinator = coordinator.clone();
        let sessions = SessionHandler::new(
            SessionConfig::default().with_reset_commands(Vec::new()),
            move |instance: &echo_agent::channels::ChannelSessionInstance| {
                let handler = Arc::new(
                    super::AppChannelMessageHandler::new(
                        factory_state.clone(),
                        Arc::new(crate::webhook::WebhookEmitter::new()),
                        factory_foreground.clone(),
                        instance.clone(),
                        factory_coordinator.clone(),
                    )
                    .with_bound_control_runtime(bound.clone())
                    .with_weixin_delivery(delivery.clone())
                    .with_weixin_credential_identity("account", "generation"),
                );
                *factory_captured
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(handler.clone());
                Box::new(SharedChannelHandler(handler)) as Box<dyn MessageHandler>
            },
        )
        .with_durable_ingress();
        sessions.bind_durable_ingress(echo_agent::agent::CancellationToken::new())?;
        let target = encode_target("account", "peer")?;
        seed_absent_weixin_context(protocol_store.as_ref(), "generation", "root-input").await?;
        let (publisher, receipt) = channel_ingress_receipt();
        sessions
            .admit_with_ingress(
                InboundMessage::new(
                    "weixin",
                    &target,
                    &target,
                    ChatType::Direct,
                    "begin original root",
                    "root-input",
                ),
                echo_agent::agent::CancellationToken::new(),
                publisher,
            )
            .await?;
        assert_eq!(receipt.await?, ChannelIngressDisposition::DurableInput);
        // The real original pump drives a real pooled AgentHandle. The only
        // substituted boundary is LLM output, held until its actual cancel token.
        tokio::time::timeout(std::time::Duration::from_secs(5), llm.wait_started())
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "24-steer main-stream start deadline: {error}; routing={}, main={}",
                    llm.routing_calls.load(std::sync::atomic::Ordering::Acquire),
                    llm.calls.load(std::sync::atomic::Ordering::Acquire)
                )
            })?;
        eprintln!(
            "CHANNEL24_PHASE:original-main-stream-started routing={} main={}",
            llm.routing_calls.load(std::sync::atomic::Ordering::Acquire),
            llm.calls.load(std::sync::atomic::Ordering::Acquire)
        );
        let handler = captured
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| anyhow::anyhow!("actual handler missing"))?;
        let conversation =
            super::AppChannelMessageHandler::conversation_id("weixin", &target, &target);
        let address =
            super::channel_input_address(runtime.execution_scope().workspace_id(), &conversation);
        let mut prompts = handler.hitl.subscribe_prompts();
        let provider = handler.hitl.clone();
        let pending = tokio::spawn(async move {
            provider
                .request(HumanLoopRequest::input(
                    "Original active root requests phone confirmation",
                ))
                .await
        });
        let prompt = tokio::time::timeout(std::time::Duration::from_secs(2), prompts.recv())
            .await
            .map_err(|error| anyhow::anyhow!("24-steer HITL prompt deadline: {error}"))??;
        eprintln!("CHANNEL24_PHASE:original-hitl-prompt");
        let response_target = prompt
            .split("Use /reply ")
            .nth(1)
            .and_then(|text| text.split_whitespace().next())
            .ok_or_else(|| anyhow::anyhow!("original phone target missing"))?;
        for ordinal in 0..24_u32 {
            seed_absent_weixin_context(
                protocol_store.as_ref(),
                "generation",
                &format!("steer-{ordinal}"),
            )
            .await?;
            let (publisher, receipt) = channel_ingress_receipt();
            sessions
                .admit_with_ingress(
                    InboundMessage::new(
                        "weixin",
                        &target,
                        &target,
                        ChatType::Direct,
                        format!("/steer actual instruction {ordinal}"),
                        format!("steer-{ordinal}"),
                    ),
                    echo_agent::agent::CancellationToken::new(),
                    publisher,
                )
                .await?;
            // First live input reaches route_live_conversation_input and real
            // AgentHandle::steer_input_tracked. Ordered successors may remain in
            // the existing durable frontier, with no per-request output waiter.
            assert_eq!(receipt.await?, ChannelIngressDisposition::DurableInput);
        }
        assert!(
            !pending.is_finished(),
            "all SessionHandler ingress calls must finish before phone HITL is answered"
        );
        let original_tracked_identity =
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let frontier = state.conversation_inputs().list(&address).await?;
                    if let Some(tracked) = frontier.items.iter().find(|input| {
                        super::channel_input_effective_text(&input.payload).starts_with("actual instruction ")
                        && input.receipt.phase
                            == crate::conversation_input::ConversationInputPhase::MailboxAccepted
                    }) {
                        let steers = frontier
                            .items
                            .iter()
                            .filter(|input| super::channel_input_effective_text(&input.payload).starts_with("actual instruction "))
                            .collect::<Vec<_>>();
                        assert_eq!(
                            steers.len(),
                            24,
                            "all real ingress inputs must remain durably represented"
                        );
                        assert!(
                            steers.iter().all(|input| !input.receipt.drained),
                            "durable ingress ACK cannot invent model drain"
                        );
                        break Ok::<_, crate::conversation_input::ConversationInputError>(
                            tracked.receipt.identity.clone(),
                        );
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await.map_err(|error| anyhow::anyhow!("24-steer original mailbox acceptance deadline: {error}; routing={}, main={}",
                llm.routing_calls.load(std::sync::atomic::Ordering::Acquire), llm.calls.load(std::sync::atomic::Ordering::Acquire)))??;
        eprintln!("CHANNEL24_PHASE:all-24-durable-original-mailbox-accepted");
        seed_absent_weixin_context(
            protocol_store.as_ref(),
            "generation",
            "phone-after-24-steers",
        )
        .await?;
        let (publisher, receipt) = channel_ingress_receipt();
        sessions
            .admit_with_ingress(
                InboundMessage::new(
                    "weixin",
                    &target,
                    &target,
                    ChatType::Direct,
                    format!("/reply {response_target} continue"),
                    "phone-after-24-steers",
                ),
                echo_agent::agent::CancellationToken::new(),
                publisher,
            )
            .await?;
        assert_eq!(receipt.await?, ChannelIngressDisposition::ControlHandled);
        assert!(matches!(pending.await??, HumanLoopResponse::Text(text) if text == "continue"));
        let root = foreground
            .snapshot_scoped(
                runtime.execution_scope().workspace_id(),
                crate::foreground_turn::ForegroundTurnSurface::Channel,
                &conversation,
            )
            .ok_or_else(|| {
                anyhow::anyhow!("original foreground disappeared before physical cancellation")
            })?
            .root_turn_id;
        coordinator
            .begin_input_pump_shutdown()
            .map_err(anyhow::Error::msg)?;
        foreground
            .request_root_cancel_scoped(
                runtime.execution_scope().workspace_id(),
                crate::foreground_turn::ForegroundTurnSurface::Channel,
                &conversation,
                &root,
            )?
            .wait()
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            // The original SessionHandler owns durable service cancellation and stream retirement.
            // Close that admission before observing the coordinator's original pump EOF.
            sessions.close().await?;
            coordinator
                .join_input_pumps()
                .await
                .map_err(anyhow::Error::msg)
        })
        .await
        .map_err(|error| {
            anyhow::anyhow!("24-steer original session close/pump join deadline: {error}")
        })??;
        let after = state.conversation_inputs().list(&address).await?;
        let original_tracked = after
            .items
            .iter()
            .find(|input| input.receipt.identity == original_tracked_identity)
            .ok_or_else(|| {
                anyhow::anyhow!("original tracked input lost after physical settlement")
            })?;
        assert!(
            matches!(
                original_tracked.receipt.phase,
                crate::conversation_input::ConversationInputPhase::TurnSettled
                    | crate::conversation_input::ConversationInputPhase::RecoveryRequired
            ),
            "original supervisor must record actual settlement/recovery after cancellation"
        );
        if original_tracked.receipt.phase
            == crate::conversation_input::ConversationInputPhase::TurnSettled
        {
            assert_eq!(
                original_tracked.receipt.outcome,
                Some(crate::conversation_input::ConversationInputOutcome::Cancelled)
            );
            assert!(
                !original_tracked.receipt.drained,
                "the held LLM never consumed its mailbox before root cancellation"
            );
        }
        assert_eq!(
            after
                .items
                .iter()
                .filter(|input| super::channel_input_effective_text(&input.payload)
                    .starts_with("actual instruction "))
                .count(),
            24
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn forty_deferred_weixin_inputs_release_ingress_and_phone_reply_still_resolves()
    -> anyhow::Result<()> {
        use echo_agent::channels::weixin::{
            WeixinChannel, WeixinConfig, WeixinCredentials, encode_target,
        };
        use echo_agent::channels::{
            ChannelIngressDisposition, ChatType, InboundMessage, MessageHandler, SessionConfig,
            SessionHandler, channel_ingress_receipt,
        };
        use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
        let temporary = tempfile::tempdir()?;
        let mut state = crate::state::AppState::from_shared(
            channel_test_agent(std::time::Duration::ZERO).map_err(anyhow::Error::msg)?,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                temporary.path().join("mcp.json"),
                echo_agent::mcp::McpConfigFile::default(),
            )),
            crate::product_data_io::ProductDataIoService::new(),
        )?;
        state.storage.chat_events = Arc::new(crate::chat_event_log::ChatEventLog::open(
            temporary.path().join("chat-events"),
            crate::chat_event_log::ChatEventRetention::default(),
        )?);
        let state = Arc::new(state);
        let original = state.current_control_runtime().await?;
        let bound = original.clone();
        // Construct the real producer handle without start: the test never
        // authenticates, polls or sends, and queued values create no HTTP work.
        let protocol_store = Arc::new(echo_agent::memory::InMemoryStore::new());
        let channel = WeixinChannel::new(WeixinConfig {
            credentials: WeixinCredentials {
                account_id: "account".to_string(),
                bot_token: "offline-fixture".to_string(),
                base_url: "https://example.invalid".to_string(),
                user_id: "peer".to_string(),
                generation: "original-generation".to_string(),
            },
            store: protocol_store.clone(),
        })?;
        let delivery = channel
            .delivery_handle()
            .ok_or_else(|| anyhow::anyhow!("original producer handle missing"))?;
        let captured = Arc::new(std::sync::Mutex::new(None));
        let factory_captured = captured.clone();
        let pump_owner = Arc::new(std::sync::Mutex::new(None));
        let factory_pump_owner = pump_owner.clone();
        let factory_state = state.clone();
        let coordinator = Arc::new(super::ChannelSessionCoordinator::new());
        let factory_coordinator = coordinator.clone();
        let foreground = crate::foreground_turn::ForegroundTurnControl::default();
        let factory_foreground = foreground.clone();
        let sessions = SessionHandler::new(
            SessionConfig::default(),
            move |instance: &echo_agent::channels::ChannelSessionInstance| {
                let handler = Arc::new(
                    super::AppChannelMessageHandler::new(
                        factory_state.clone(),
                        Arc::new(crate::webhook::WebhookEmitter::new()),
                        factory_foreground.clone(),
                        instance.clone(),
                        factory_coordinator.clone(),
                    )
                    .with_bound_control_runtime(bound.clone())
                    .with_weixin_delivery(delivery.clone())
                    .with_weixin_credential_identity("account", "original-generation"),
                );
                if let Ok(super::input_pump::ChannelInputPumpKick::Started(owner)) =
                    handler.input_pump.kick()
                {
                    *factory_pump_owner
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(owner);
                }
                *factory_captured
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(handler.clone());
                Box::new(SharedChannelHandler(handler)) as Box<dyn MessageHandler>
            },
        )
        .with_durable_ingress();
        sessions.bind_durable_ingress(echo_agent::agent::CancellationToken::new())?;
        let target = encode_target("account", "peer")?;
        // Every message goes through the REAL lazy SessionHandler wrapper.
        // The factory holds the original elected pump owner before first input.
        for ordinal in 0..40_u32 {
            seed_absent_weixin_context(
                protocol_store.as_ref(),
                "original-generation",
                &format!("provider-{ordinal}"),
            )
            .await?;
            let (receipt, received) = channel_ingress_receipt();
            sessions
                .admit_with_ingress(
                    InboundMessage::new(
                        "weixin",
                        &target,
                        &target,
                        ChatType::Direct,
                        format!("queued {ordinal}"),
                        format!("provider-{ordinal}"),
                    ),
                    echo_agent::agent::CancellationToken::new(),
                    receipt,
                )
                .await?;
            assert_eq!(received.await?, ChannelIngressDisposition::DurableInput);
        }
        let handler = captured
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| anyhow::anyhow!("actual handler missing"))?;
        assert!(
            pump_owner
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some()
        );
        assert!(handler.has_pending_work());
        assert!(
            coordinator
                .input_pump_tasks
                .lock()
                .map_err(|_| anyhow::anyhow!("task owner poisoned"))?
                .tasks
                .lock()
                .map_err(|_| anyhow::anyhow!("join set poisoned"))?
                .is_empty(),
            "queued responses must not create tasks"
        );
        let conv = super::AppChannelMessageHandler::conversation_id("weixin", &target, &target);
        let address =
            super::channel_input_address(original.execution_scope().workspace_id(), &conv);
        assert_eq!(
            state
                .conversation_inputs()
                .list(&address)
                .await?
                .items
                .len(),
            40
        );
        let agent_conv = super::AppChannelMessageHandler::agent_conversation_id(
            &conv,
            &handler.session_instance.incarnation_id(),
        );
        let surface = super::ChannelSessionCoordinator::surface_id(&handler.session_instance);
        let lease = foreground.begin_scoped(
            original.execution_scope().workspace_id(),
            crate::foreground_turn::ForegroundTurnSurface::Channel,
            &conv,
            "original-phone-request",
        )?;
        handler
            .publish_active_turn(
                &surface,
                &conv,
                &agent_conv,
                &original,
                "original-phone-request",
            )
            .map_err(anyhow::Error::msg)?;
        lease.retain(super::ChannelActiveTurnOwner::new(
            coordinator.active_turns.clone(),
            surface,
            "original-phone-request".to_string(),
        ));
        let mut prompts = handler.hitl.subscribe_prompts();
        let provider = handler.hitl.clone();
        let pending = tokio::spawn(async move {
            provider
                .request(HumanLoopRequest::input("Continue original request?"))
                .await
        });
        let prompt =
            tokio::time::timeout(std::time::Duration::from_secs(2), prompts.recv()).await??;
        let response_target = prompt
            .split("Use /reply ")
            .nth(1)
            .and_then(|suffix| suffix.split_whitespace().next())
            .ok_or_else(|| anyhow::anyhow!("original HITL response target missing"))?;
        seed_absent_weixin_context(
            protocol_store.as_ref(),
            "original-generation",
            "phone-reply-after-forty",
        )
        .await?;
        let (receipt, received) = channel_ingress_receipt();
        sessions
            .admit_with_ingress(
                InboundMessage::new(
                    "weixin",
                    &target,
                    &target,
                    ChatType::Direct,
                    format!("/reply {response_target} accepted"),
                    "phone-reply-after-forty",
                ),
                echo_agent::agent::CancellationToken::new(),
                receipt,
            )
            .await?;
        assert_eq!(received.await?, ChannelIngressDisposition::ControlHandled);
        assert!(
            matches!(tokio::time::timeout(std::time::Duration::from_secs(2), pending).await???, HumanLoopResponse::Text(ref answer) if answer == "accepted")
        );
        assert_eq!(
            state
                .conversation_inputs()
                .list(&address)
                .await?
                .items
                .len(),
            40
        );
        handler
            .input_pump
            .begin_shutdown()
            .map_err(anyhow::Error::msg)?;
        drop(
            pump_owner
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take(),
        );
        lease
            .settle_after_observers(crate::chat_driver::TurnOutcome::Cancelled)
            .await?;
        assert!(!handler.has_pending_work());
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn weixin_captured_runtime_survives_gui_focus_transition() -> anyhow::Result<()> {
        use echo_agent::channels::{
            ChatType, InboundMessage, MessageHandler, SessionConfig, SessionHandler,
        };
        let temporary = tempfile::tempdir()?;
        let state = Arc::new(crate::state::AppState::from_shared(
            channel_test_agent(std::time::Duration::ZERO).map_err(anyhow::Error::msg)?,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                temporary.path().join("mcp.json"),
                echo_agent::mcp::McpConfigFile::default(),
            )),
            crate::product_data_io::ProductDataIoService::new(),
        )?);
        let original = state.current_control_runtime().await?;
        let expected = original.execution_scope().workspace_id().to_string();
        let captured = Arc::new(std::sync::Mutex::new(None));
        let factory_captured = captured.clone();
        let factory_state = state.clone();
        let sessions = SessionHandler::new(
            SessionConfig::default(),
            move |instance: &echo_agent::channels::ChannelSessionInstance| {
                let handler = super::AppChannelMessageHandler::new(
                    factory_state.clone(),
                    Arc::new(crate::webhook::WebhookEmitter::new()),
                    crate::foreground_turn::ForegroundTurnControl::default(),
                    instance.clone(),
                    Arc::new(super::ChannelSessionCoordinator::new()),
                )
                .with_bound_control_runtime(original.clone());
                *factory_captured
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(handler);
                Box::new(SessionIdentityProbe) as Box<dyn MessageHandler>
            },
        );
        sessions
            .handle(InboundMessage::new(
                "weixin",
                "peer",
                "account-peer",
                ChatType::Direct,
                "fixture",
                "fixture",
            ))
            .await?;
        let handler = captured
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .ok_or_else(|| anyhow::anyhow!("channel handler was not constructed"))?;
        // During GUI publication the global resolver deliberately fails closed;
        // the already-admitted channel keeps its original generation receipt.
        state.set_workspace_transition_for_test(true);
        assert!(state.workspace_transition_in_progress());
        assert!(state.current_control_runtime().await.is_err());
        assert_eq!(
            handler
                .control_runtime()
                .await?
                .execution_scope()
                .workspace_id(),
            expected
        );
        state.set_workspace_transition_for_test(false);
        assert!(!state.workspace_transition_in_progress());
        Ok(())
    }

    #[cfg(feature = "channels")]
    fn channel_test_agent(
        delay: std::time::Duration,
    ) -> Result<echo_agent::agent::AgentHandle, String> {
        let llm = std::sync::Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("channel-scope-test")
                .with_response("done")
                .with_delay(delay),
        );
        echo_agent::agent::ReactAgentBuilder::new()
            .model("channel-scope-test")
            .llm_client(llm)
            .build()
            .map(echo_agent::agent::AgentHandle::new)
            .map_err(|error| error.to_string())
    }

    #[cfg(feature = "channels")]
    struct ChannelCancellationBarrierLlmClient {
        calls: std::sync::atomic::AtomicUsize,
        routing_calls: std::sync::atomic::AtomicUsize,
        complete_intent_classification: bool,
        started: std::sync::atomic::AtomicBool,
        started_notify: tokio::sync::Notify,
    }

    #[cfg(feature = "channels")]
    impl ChannelCancellationBarrierLlmClient {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
                routing_calls: std::sync::atomic::AtomicUsize::new(0),
                complete_intent_classification: false,
                started: std::sync::atomic::AtomicBool::new(false),
                started_notify: tokio::sync::Notify::new(),
            }
        }

        fn after_intent_router() -> Self {
            Self {
                complete_intent_classification: true,
                ..Self::new()
            }
        }

        async fn wait_started(&self) {
            if self.complete_intent_classification {
                let notified = self.started_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if !self.started.load(std::sync::atomic::Ordering::Acquire) {
                    notified.await;
                }
            } else {
                if self.started.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                self.started_notify.notified().await;
            }
        }

        async fn wait_for_cancel(
            &self,
            request: &echo_agent::llm::ChatRequest,
        ) -> echo_agent::error::Result<()> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            self.started
                .store(true, std::sync::atomic::Ordering::Release);
            self.started_notify.notify_waiters();
            request
                .cancel_token
                .clone()
                .unwrap_or_default()
                .cancelled()
                .await;
            Err(echo_agent::error::ReactError::Agent(Box::new(
                echo_agent::error::AgentError::Cancelled(
                    "channel cancellation barrier observed root cancel".to_string(),
                ),
            )))
        }
    }

    #[cfg(feature = "channels")]
    impl echo_agent::llm::LlmClient for ChannelCancellationBarrierLlmClient {
        fn chat(
            &self,
            request: echo_agent::llm::ChatRequest,
        ) -> futures::future::BoxFuture<'_, echo_agent::error::Result<echo_agent::llm::ChatResponse>>
        {
            Box::pin(async move {
                if !self.complete_intent_classification {
                    self.wait_for_cancel(&request).await?;
                    return Err(echo_agent::error::ReactError::Other(
                        "channel cancellation barrier returned unexpectedly".to_string(),
                    ));
                }
                // Only the 24-steer fixture completes intent before parking its original ReAct stream.
                let classifier = request.messages.len() == 1
                    && request.messages.first().is_some_and(|message| {
                        message.role == echo_agent::llm::types::Role::User
                            && message.text_content().is_some_and(|text| {
                                text.starts_with(
                                    "You are an intent classifier for an AI agent system.",
                                )
                            })
                    });
                if classifier {
                    self.routing_calls
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    return echo_agent::testing::MockLlmClient::new()
                        .with_response(r#"{"intent":"FALLBACK","skill":null,"confidence":1.0}"#)
                        .chat(request)
                        .await;
                }
                Err(echo_agent::error::ReactError::Other(
                    "unexpected non-classifier chat request before the held original stream closes"
                        .to_string(),
                ))
            })
        }

        fn chat_stream(
            &self,
            request: echo_agent::llm::ChatRequest,
        ) -> futures::future::BoxFuture<
            '_,
            echo_agent::error::Result<
                futures::stream::BoxStream<
                    'static,
                    echo_agent::error::Result<echo_agent::llm::ChatChunk>,
                >,
            >,
        > {
            Box::pin(async move {
                self.wait_for_cancel(&request).await?;
                Err(echo_agent::error::ReactError::Other(
                    "channel cancellation barrier returned unexpectedly".to_string(),
                ))
            })
        }

        fn model_name(&self) -> &str {
            "channel-cancellation-barrier"
        }
    }

    #[cfg(feature = "channels")]
    #[test]
    fn channel_budget_parser_accepts_positive_or_unbounded() {
        assert_eq!(super::parse_channel_budget("42", "token"), Ok(Some(42)));
        assert_eq!(super::parse_channel_budget("unbounded", "time"), Ok(None));
        assert!(super::parse_channel_budget("0", "time").is_err());
    }

    #[cfg(feature = "channels")]
    #[test]
    fn resume_dispatch_separates_planned_and_continuation_authorities() -> Result<(), String> {
        use crate::tasks::task_runtime::{RunTurnOrigin, TaskRunResumeIdentity};

        let identity = TaskRunResumeIdentity {
            run_id: "run-a".to_string(),
            workspace_id: "workspace-a".to_string(),
            conversation_id: "conversation-a".to_string(),
            root_message_id: "root-a".to_string(),
            created_at: chrono::Utc::now(),
            goal_revision: 3,
            journal_sequence: 11,
            continuation_enabled: true,
            execution_profile:
                crate::tasks::task_runtime::TaskRunExecutionProfile::conversation_turn(),
            project_binding: None,
        };
        assert!(matches!(
            super::channel_resume_dispatch(identity.clone(), false, "surface-turn"),
            super::ChannelResumeDispatch::Planned(ref planned) if planned == &identity
        ));
        let continuation = super::channel_resume_dispatch(identity.clone(), true, "surface-turn");
        let super::ChannelResumeDispatch::Continuation(binding) = continuation else {
            return Err("continuation-enabled resume selected planned execution".to_string());
        };
        assert_eq!(binding.run_id.as_deref(), Some("run-a"));
        assert_eq!(binding.turn_id, "surface-turn");
        assert_eq!(binding.root_message_id, "root-a");
        assert_eq!(binding.origin, RunTurnOrigin::Resume);
        assert_eq!(binding.expected_resume.as_ref(), Some(&identity));
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn resume_rejects_new_attachments_before_admission() {
        assert!(!super::channel_resume_rejects_attachments(false, 0));
        assert!(!super::channel_resume_rejects_attachments(false, 2));
        assert!(!super::channel_resume_rejects_attachments(true, 0));
        assert!(super::channel_resume_rejects_attachments(true, 1));
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn completed_channel_operation_wins_simultaneous_cancel_safe_point() {
        let cancel = echo_agent::agent::CancellationToken::new();
        cancel.cancel();
        let outcome = super::await_channel_operation(cancel, async { "committed" }).await;
        assert_eq!(outcome, Some("committed"));
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn reset_gc_removes_only_the_exact_sender_incarnation() -> Result<(), String> {
        use echo_agent::memory::{ConversationStore, FileConversationStore, NewConversation};
        use echo_agent::state::{AgentCheckpoint, FileRuntimeStateStore, RuntimeStateStore};

        let temp =
            std::env::temp_dir().join(format!("eko-channel-reset-gc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).map_err(|error| error.to_string())?;
        let conversations: std::sync::Arc<dyn ConversationStore> = std::sync::Arc::new(
            FileConversationStore::new(temp.join("conversations"))
                .map_err(|error| error.to_string())?,
        );
        let runtime_state: std::sync::Arc<dyn RuntimeStateStore> = std::sync::Arc::new(
            FileRuntimeStateStore::new(temp.join("runtime")).map_err(|error| error.to_string())?,
        );
        let product_id = "channel-product-alice";
        let retired_id = "channel-runtime-alice-a";
        let retained_id = "channel-runtime-alice-b";
        let other_product_id = "channel-product-bob";
        let other_runtime_id = "channel-runtime-bob-a";
        for conversation_id in [
            product_id,
            retired_id,
            retained_id,
            other_product_id,
            other_runtime_id,
        ] {
            conversations
                .create_conversation(NewConversation {
                    conversation_id: conversation_id.to_string(),
                    user_id: "default".to_string(),
                    agent_type: None,
                    title: None,
                })
                .await
                .map_err(|error| error.to_string())?;
        }
        for (scope_id, runtime_id) in [
            (product_id, retired_id),
            (product_id, retained_id),
            (other_product_id, other_runtime_id),
        ] {
            let mut checkpoint = AgentCheckpoint::new(runtime_id);
            checkpoint.messages_json = AgentCheckpoint::serialize_payload(Vec::new(), None)
                .map_err(|error| error.to_string())?;
            runtime_state
                .save_checkpoint_for_scope(scope_id, &checkpoint)
                .await
                .map_err(|error| error.to_string())?;
        }

        assert!(
            super::AppChannelMessageHandler::clear_runtime_incarnation_stores(
                Some(conversations.clone()),
                Some(runtime_state.clone()),
                product_id,
                retired_id,
            )
            .await?
        );
        assert!(
            conversations
                .get_conversation(product_id)
                .await
                .map_err(|error| error.to_string())?
                .is_some()
        );
        assert!(
            conversations
                .get_conversation(retired_id)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert!(
            runtime_state
                .get_checkpoint(retired_id)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert!(
            runtime_state
                .get_checkpoint(retained_id)
                .await
                .map_err(|error| error.to_string())?
                .is_some()
        );
        assert!(
            runtime_state
                .get_checkpoint(other_runtime_id)
                .await
                .map_err(|error| error.to_string())?
                .is_some()
        );
        let _cleanup = std::fs::remove_dir_all(temp);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn session_retirement_marks_every_exact_workspace_owner_for_the_incarnation() {
        let gate = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
        let obligation = |workspace_id: &str,
                          workspace_generation: &str,
                          runtime_state_id: &str,
                          incarnation_id: &str| {
            let key = super::ChannelRuntimeOwnerKey {
                workspace_id: workspace_id.to_string(),
                workspace_generation: workspace_generation.to_string(),
                runtime_state_id: runtime_state_id.to_string(),
            };
            (
                key.clone(),
                super::ChannelRuntimeObligation {
                    key,
                    product_conversation_id: "stable-product".to_string(),
                    incarnation_id: incarnation_id.to_string(),
                    phase: super::ChannelRetirementPhase::Active,
                },
            )
        };
        {
            let mut obligations = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            obligations.extend([
                obligation("workspace-a", "generation-a", "agent-runtime", "inc-a"),
                obligation("workspace-b", "generation-b", "agent-runtime", "inc-a"),
                obligation("workspace-c", "generation-c", "agent-runtime", "inc-b"),
            ]);
        }

        super::ChannelSessionCoordinator::mark_incarnation_pending(&gate, "inc-a");

        let obligations = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let pending = obligations
            .values()
            .filter(|obligation| obligation.phase == super::ChannelRetirementPhase::RetirePending)
            .map(|obligation| {
                (
                    obligation.key.workspace_id.as_str(),
                    obligation.key.workspace_generation.as_str(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            pending,
            std::collections::BTreeSet::from([
                ("workspace-a", "generation-a"),
                ("workspace-b", "generation-b"),
            ])
        );
        assert!(obligations.values().any(|obligation| {
            obligation.incarnation_id == "inc-b"
                && obligation.phase == super::ChannelRetirementPhase::Active
        }));
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn handler_gc_failure_and_cancel_keep_the_exact_obligation_for_retry()
    -> Result<(), String> {
        let key = super::ChannelRuntimeOwnerKey {
            workspace_id: "workspace-a".to_string(),
            workspace_generation: "generation-a".to_string(),
            runtime_state_id: "runtime-a".to_string(),
        };
        let gate =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::from([(
                key.clone(),
                super::ChannelRuntimeObligation {
                    key: key.clone(),
                    product_conversation_id: "product-a".to_string(),
                    incarnation_id: "incarnation-a".to_string(),
                    phase: super::ChannelRetirementPhase::RetirePending,
                },
            )])));

        let failed =
            super::await_channel_retirement(echo_agent::agent::CancellationToken::new(), async {
                Err::<(), _>("injected GC failure".to_string())
            })
            .await;
        assert!(matches!(
            failed,
            Err(super::ChannelSessionRetirementError::Failed(_))
        ));
        assert!(
            gate.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains_key(&key)
        );

        let cancel = echo_agent::agent::CancellationToken::new();
        cancel.cancel();
        let cancelled = super::await_channel_retirement(cancel, async {
            futures::future::pending::<Result<(), String>>().await
        })
        .await;
        assert_eq!(
            cancelled,
            Err(super::ChannelSessionRetirementError::Cancelled)
        );
        let mut obligations = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let retained = obligations
            .get_mut(&key)
            .ok_or_else(|| "cancelled GC consumed the exact obligation".to_string())?;
        retained.phase = super::ChannelRetirementPhase::GcPending;
        obligations.remove(&key);
        assert!(obligations.is_empty());
        Ok::<(), String>(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn product_identity_is_stable_while_agent_and_cache_follow_incarnation() {
        let alice =
            super::AppChannelMessageHandler::conversation_id("qqbot", "shared-group", "alice");
        let alice_again =
            super::AppChannelMessageHandler::conversation_id("qqbot", "shared-group", "alice");
        let bob = super::AppChannelMessageHandler::conversation_id("qqbot", "shared-group", "bob");
        let other_chat =
            super::AppChannelMessageHandler::conversation_id("qqbot", "other", "alice");
        let other_channel =
            super::AppChannelMessageHandler::conversation_id("feishu", "shared-group", "alice");
        assert_eq!(alice, alice_again);
        assert!(alice.starts_with("channel:sha256:"));
        assert_ne!(alice, bob);
        assert_ne!(alice, other_chat);
        assert_ne!(alice, other_channel);

        let alice_agent_a =
            super::AppChannelMessageHandler::agent_conversation_id(&alice, "incarnation-a");
        let alice_agent_b =
            super::AppChannelMessageHandler::agent_conversation_id(&alice, "incarnation-b");
        let bob_agent =
            super::AppChannelMessageHandler::agent_conversation_id(&bob, "incarnation-a");
        assert!(alice_agent_a.starts_with("channel-runtime:sha256:"));
        assert_ne!(alice_agent_a, alice_agent_b);
        assert_ne!(alice_agent_a, bob_agent);

        let alice_cache_a = super::AppChannelMessageHandler::cache_user_id(&alice, "incarnation-a");
        let alice_cache_b = super::AppChannelMessageHandler::cache_user_id(&alice, "incarnation-b");
        assert!(alice_cache_a.starts_with("im-"));
        assert_ne!(alice_cache_a, alice_cache_b);
    }

    #[cfg(feature = "channels")]
    #[test]
    fn session_end_replaces_only_the_exact_surface_pump_slot() -> Result<(), String> {
        let coordinator = super::ChannelSessionCoordinator::new();
        let alice = super::ChannelSurfaceIdentity {
            channel_id: "qq".to_string(),
            chat_id: "group".to_string(),
            sender_id: "alice".to_string(),
        };
        let bob = super::ChannelSurfaceIdentity {
            sender_id: "bob".to_string(),
            ..alice.clone()
        };
        let alice_old = coordinator.input_pump(&alice);
        let bob_slot = coordinator.input_pump(&bob);
        coordinator
            .lifecycle
            .lock()
            .map_err(|_| "channel lifecycle test registry is unavailable".to_string())?
            .entry(alice.clone())
            .or_default()
            .current_incarnation_id = Some("ended-incarnation".to_string());
        coordinator.record_session_end(echo_agent::channels::SessionEndInfo {
            channel_id: alice.channel_id.clone(),
            chat_id: alice.chat_id.clone(),
            sender_id: alice.sender_id.clone(),
            incarnation_id: "ended-incarnation".to_string(),
            reason: echo_agent::channels::SessionEndReason::TimeoutReplaced,
        });
        let alice_new = coordinator.input_pump(&alice);
        assert!(!Arc::ptr_eq(&alice_old, &alice_new));
        assert!(Arc::ptr_eq(&bob_slot, &coordinator.input_pump(&bob)));
        assert!(alice_old.kick().is_err());
        assert!(alice_new.kick().is_ok());
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn stale_session_end_does_not_shutdown_the_replacement_pump() -> Result<(), String> {
        let coordinator = super::ChannelSessionCoordinator::new();
        let surface = super::ChannelSurfaceIdentity {
            channel_id: "qq".to_string(),
            chat_id: "group".to_string(),
            sender_id: "alice".to_string(),
        };
        coordinator
            .lifecycle
            .lock()
            .map_err(|_| "channel lifecycle test registry is unavailable".to_string())?
            .entry(surface.clone())
            .or_default()
            .current_incarnation_id = Some("replacement".to_string());
        let replacement = coordinator.input_pump(&surface);

        coordinator.record_session_end(echo_agent::channels::SessionEndInfo {
            channel_id: surface.channel_id.clone(),
            chat_id: surface.chat_id.clone(),
            sender_id: surface.sender_id.clone(),
            incarnation_id: "retired".to_string(),
            reason: echo_agent::channels::SessionEndReason::TimeoutReplaced,
        });

        assert!(Arc::ptr_eq(&replacement, &coordinator.input_pump(&surface)));
        assert!(replacement.kick().is_ok());
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn active_surface_identity_is_typed_and_delimiter_safe() {
        let left =
            super::AppChannelMessageHandler::active_surface_identity("qq", "a:sender:b", "c");
        let right =
            super::AppChannelMessageHandler::active_surface_identity("qq", "a", "b:sender:c");
        assert_ne!(left, right);
        let identities = std::collections::HashSet::from([left, right]);
        assert_eq!(identities.len(), 2);
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn late_framework_end_callback_cannot_clear_registered_replacement() -> Result<(), String>
    {
        use echo_agent::channels::{
            ChatType, InboundMessage, MessageHandler, SessionConfig, SessionHandler,
        };

        let coordinator = std::sync::Arc::new(super::ChannelSessionCoordinator::new());
        let factory_coordinator = std::sync::Arc::clone(&coordinator);
        let end_coordinator = std::sync::Arc::clone(&coordinator);
        let instances = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let factory_instances = std::sync::Arc::clone(&instances);
        let handler = SessionHandler::new(
            SessionConfig::default(),
            move |instance: &echo_agent::channels::ChannelSessionInstance| {
                let registration = factory_coordinator.register(instance);
                match factory_instances.lock() {
                    Ok(mut instances) => instances.push((instance.clone(), registration)),
                    Err(poisoned) => poisoned.into_inner().push((instance.clone(), registration)),
                }
                Box::new(SessionIdentityProbe) as Box<dyn MessageHandler>
            },
        )
        .with_on_session_end(move |info| end_coordinator.record_session_end(info));
        let message = |text: &str, id: &str| {
            InboundMessage::new(
                "channel",
                "sender",
                "conversation",
                ChatType::Direct,
                text,
                id,
            )
        };

        handler
            .handle(message("first", "m1"))
            .await
            .map_err(|error| error.to_string())?;
        handler
            .handle(message("reset chat", "m2"))
            .await
            .map_err(|error| error.to_string())?;
        let captured = match instances.lock() {
            Ok(instances) => instances.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        if captured.len() != 2 || captured.iter().any(|(_, result)| result.is_err()) {
            return Err("framework replacement did not register both incarnations".to_string());
        }
        let replacement = captured
            .get(1)
            .map(|(instance, _)| instance.clone())
            .ok_or_else(|| "replacement instance is missing".to_string())?;
        let surface_id = super::ChannelSessionCoordinator::surface_id(&replacement);
        let lifecycle = coordinator
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = lifecycle
            .get(&surface_id)
            .ok_or_else(|| "replacement lifecycle record is missing".to_string())?;
        if record.current_incarnation_id.as_deref() != Some(replacement.incarnation_id().as_str())
            || record.pending_ended_incarnation_id.is_some()
        {
            return Err("late end callback cleared the replacement incarnation".to_string());
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    type SessionRegistrations =
        std::sync::Arc<std::sync::Mutex<Vec<(String, Result<Option<String>, String>)>>>;

    #[cfg(feature = "channels")]
    fn coordinator_session_handler(
        coordinator: &std::sync::Arc<super::ChannelSessionCoordinator>,
        registrations: &SessionRegistrations,
    ) -> echo_agent::channels::SessionHandler {
        use echo_agent::channels::{MessageHandler, SessionConfig, SessionHandler};

        let factory_coordinator = std::sync::Arc::clone(coordinator);
        let end_coordinator = std::sync::Arc::clone(coordinator);
        let factory_registrations = std::sync::Arc::clone(registrations);
        SessionHandler::new(
            SessionConfig::default(),
            move |instance: &echo_agent::channels::ChannelSessionInstance| {
                let registration = factory_coordinator
                    .register(instance)
                    .map(|(previous, _gate)| previous);
                match factory_registrations.lock() {
                    Ok(mut entries) => entries.push((instance.incarnation_id(), registration)),
                    Err(poisoned) => poisoned
                        .into_inner()
                        .push((instance.incarnation_id(), registration)),
                }
                Box::new(SessionIdentityProbe) as Box<dyn MessageHandler>
            },
        )
        .with_on_session_end(move |info| end_coordinator.record_session_end(info))
    }

    #[cfg(feature = "channels")]
    fn registered_previous(
        registrations: &SessionRegistrations,
        index: usize,
    ) -> Result<(String, Result<Option<String>, String>), String> {
        let entries = registrations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries
            .get(index)
            .cloned()
            .ok_or_else(|| format!("session registration {index} is missing"))
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn shutdown_end_releases_the_coordinator_for_the_same_surface() -> Result<(), String> {
        use echo_agent::channels::{ChatType, InboundMessage, MessageHandler};

        let coordinator = std::sync::Arc::new(super::ChannelSessionCoordinator::new());
        let registrations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let surface = super::ChannelSurfaceIdentity {
            channel_id: "channel".to_string(),
            chat_id: "conversation".to_string(),
            sender_id: "sender".to_string(),
        };
        let message = |text: &str, id: &str| {
            InboundMessage::new(
                "channel",
                "sender",
                "conversation",
                ChatType::Direct,
                text,
                id,
            )
        };

        let first = coordinator_session_handler(&coordinator, &registrations);
        first
            .handle(message("first", "m1"))
            .await
            .map_err(|error| error.to_string())?;
        let (first_incarnation, first_registration) = registered_previous(&registrations, 0)?;
        let first_previous = first_registration.clone()?;
        if first_previous.is_some() {
            return Err(format!(
                "first admission did not register a fresh incarnation: {first_previous:?}"
            ));
        }

        // Replacing a live incarnation without an exact predecessor stays refused.
        let premature = coordinator_session_handler(&coordinator, &registrations);
        premature
            .handle(message("early", "m2"))
            .await
            .map_err(|error| error.to_string())?;
        let (_, premature_registration) = registered_previous(&registrations, 1)?;
        if premature_registration.is_ok() {
            return Err("a live incarnation was replaced without a predecessor".to_string());
        }

        first.close().await.map_err(|error| error.to_string())?;
        {
            let lifecycle = coordinator
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let record = lifecycle
                .get(&surface)
                .ok_or_else(|| "closed session lifecycle record is missing".to_string())?;
            if record.current_incarnation_id.is_some()
                || record.pending_ended_incarnation_id.as_deref()
                    != Some(first_incarnation.as_str())
            {
                return Err(
                    "a successful shutdown did not release the exact incarnation".to_string(),
                );
            }
        }

        let reopened = coordinator_session_handler(&coordinator, &registrations);
        reopened
            .handle(message("second", "m3"))
            .await
            .map_err(|error| error.to_string())?;
        let (reopened_incarnation, reopened_registration) = registered_previous(&registrations, 2)?;
        let reopened_previous = reopened_registration.clone()?;
        if reopened_previous.as_deref() != Some(first_incarnation.as_str()) {
            return Err(format!(
                "reopened admission did not carry the closed predecessor: {reopened_previous:?}"
            ));
        }
        if reopened_incarnation == first_incarnation {
            return Err("reopened admission reused the closed incarnation".to_string());
        }
        let lifecycle = coordinator
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = lifecycle
            .get(&surface)
            .ok_or_else(|| "reopened lifecycle record is missing".to_string())?;
        if record.current_incarnation_id.as_deref() != Some(reopened_incarnation.as_str()) {
            return Err("reopened session did not become the current incarnation".to_string());
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn stale_coordinator_rejects_rotation_without_changing_framework_instance()
    -> Result<(), String> {
        use echo_agent::channels::{ChatType, InboundMessage, MessageHandler, SessionHandler};

        let instance_slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let factory_slot = std::sync::Arc::clone(&instance_slot);
        let handler = SessionHandler::with_defaults(
            move |instance: &echo_agent::channels::ChannelSessionInstance| {
                match factory_slot.lock() {
                    Ok(mut slot) => *slot = Some(instance.clone()),
                    Err(poisoned) => *poisoned.into_inner() = Some(instance.clone()),
                }
                Box::new(SessionIdentityProbe) as Box<dyn MessageHandler>
            },
        );
        handler
            .handle(InboundMessage::new(
                "channel",
                "sender",
                "conversation",
                ChatType::Direct,
                "first",
                "m1",
            ))
            .await
            .map_err(|error| error.to_string())?;
        let instance = match instance_slot.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
        .ok_or_else(|| "framework instance is missing".to_string())?;
        let coordinator = super::ChannelSessionCoordinator::new();
        coordinator.register(&instance)?;
        let surface_id = super::ChannelSessionCoordinator::surface_id(&instance);
        {
            let mut lifecycle = coordinator
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let record = lifecycle.entry(surface_id.clone()).or_default();
            record.current_incarnation_id = Some("stale-incarnation".to_string());
        }
        let before = instance.incarnation_id();
        if coordinator.rotate(&surface_id, &instance).is_ok() || instance.incarnation_id() != before
        {
            return Err("stale coordinator mutated the framework incarnation".to_string());
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn task_dispatcher_does_not_intercept_pinned_foreground_controls() {
        assert!(super::is_task_run_control_command("/task-resume"));
        assert!(super::is_task_run_control_command("/subagent-message"));
        assert!(!super::is_task_run_control_command("/stop"));
        assert!(!super::is_task_run_control_command("/reset"));
        assert!(!super::is_task_run_control_command("/steer"));
    }

    #[cfg(feature = "channels")]
    #[test]
    fn cancellation_barrier_preserves_pin_while_root_snapshot_is_valid() -> Result<(), String> {
        use crate::foreground_turn::{
            ForegroundTurnControl, ForegroundTurnError, ForegroundTurnSettlement,
            ForegroundTurnSurface,
        };

        let control = ForegroundTurnControl::default();
        let settled = Ok(ForegroundTurnSettlement {
            workspace_id: "workspace-a".to_string(),
            surface: ForegroundTurnSurface::Channel,
            conversation_id: "conversation-a".to_string(),
            turn_id: "root-turn".to_string(),
            outcome: crate::chat_driver::TurnOutcome::Completed,
        });
        assert!(super::channel_cancel_barrier_complete(
            &control,
            "workspace-a",
            "conversation-a",
            "root-turn",
            &settled,
        ));
        let no_active = Err(ForegroundTurnError::NoActiveTurn {
            surface: ForegroundTurnSurface::Channel,
            conversation_id: "conversation-a".to_string(),
        });
        assert!(super::channel_cancel_barrier_complete(
            &control,
            "workspace-a",
            "conversation-a",
            "root-turn",
            &no_active,
        ));

        let lease = control
            .begin_scoped(
                "workspace-a",
                ForegroundTurnSurface::Channel,
                "conversation-a",
                "root-turn",
            )
            .map_err(|error| error.to_string())?;
        assert!(!super::channel_cancel_barrier_complete(
            &control,
            "workspace-a",
            "conversation-a",
            "root-turn",
            &no_active,
        ));
        assert!(super::channel_cancel_barrier_complete(
            &control,
            "workspace-a",
            "conversation-a",
            "stale-root",
            &no_active,
        ));
        assert!(!super::channel_cancel_barrier_complete(
            &control,
            "workspace-a",
            "conversation-a",
            "root-turn",
            &Err(ForegroundTurnError::StateUnavailable),
        ));
        drop(lease);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn retirement_foreground_root_remains_stoppable_until_settlement() -> Result<(), String> {
        use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

        let control = std::sync::Arc::new(ForegroundTurnControl::default());
        let lease = control
            .begin_scoped(
                "workspace-b",
                ForegroundTurnSurface::Channel,
                "stable-product",
                "retirement-root",
            )
            .map_err(|error| error.to_string())?;
        let cancel = lease.cancellation_token();
        let cancel_control = std::sync::Arc::clone(&control);
        let stop = tokio::spawn(async move {
            super::channel_cancel_root(
                cancel_control.as_ref(),
                "workspace-b",
                "stable-product",
                "retirement-root",
            )
            .await
        });
        cancel.cancelled().await;
        assert!(
            control
                .snapshot_scoped(
                    "workspace-b",
                    ForegroundTurnSurface::Channel,
                    "stable-product",
                )
                .is_some()
        );
        lease
            .settle_after_observers(crate::chat_driver::TurnOutcome::Cancelled)
            .await
            .map_err(|error| error.to_string())?;
        stop.await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(
            control
                .snapshot_scoped(
                    "workspace-b",
                    ForegroundTurnSurface::Channel,
                    "stable-product",
                )
                .is_none()
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn planned_resume_settlement_waits_for_live_steer_terminal_projection()
    -> Result<(), String> {
        use crate::foreground_turn::{
            ForegroundTerminalProjector, ForegroundTurnControl, ForegroundTurnSurface,
        };
        use std::sync::atomic::{AtomicBool, Ordering};

        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-planned-resume",
                ForegroundTurnSurface::Channel,
                "conversation-planned-resume",
                "planned-resume-root",
            )
            .map_err(|error| error.to_string())?;
        let (release_observer, observer_release) = tokio::sync::oneshot::channel();
        let projected = Arc::new(AtomicBool::new(false));
        let projected_for_callback = Arc::clone(&projected);
        let projector: ForegroundTerminalProjector = Arc::new(move |_outcome| {
            let projected = Arc::clone(&projected_for_callback);
            Box::pin(async move {
                projected.store(true, Ordering::SeqCst);
                Ok(())
            })
        });
        control
            .supervise_input_lifecycle_scoped(
                "workspace-planned-resume",
                ForegroundTurnSurface::Channel,
                "conversation-planned-resume",
                "planned-resume-root",
                async move {
                    observer_release.await.map_err(|error| error.to_string())?;
                    Ok(())
                },
                projector,
            )
            .map_err(|error| error.to_string())?;
        let settling = tokio::spawn(async move {
            super::settle_channel_turn_after_input_observers(
                lease,
                crate::chat_driver::TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "planned_resume",
                    "injected planned resume failure",
                )),
            )
            .await
        });
        tokio::task::yield_now().await;

        assert!(!settling.is_finished());
        assert!(!projected.load(Ordering::SeqCst));
        assert!(
            control
                .snapshot_scoped(
                    "workspace-planned-resume",
                    ForegroundTurnSurface::Channel,
                    "conversation-planned-resume",
                )
                .is_some()
        );
        release_observer
            .send(())
            .map_err(|_| "planned resume observer already ended".to_string())?;
        let outcome = settling
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;

        assert!(matches!(
            outcome,
            crate::chat_driver::TurnOutcome::Failed(_)
        ));
        assert!(projected.load(Ordering::SeqCst));
        assert!(
            control
                .snapshot_scoped(
                    "workspace-planned-resume",
                    ForegroundTurnSurface::Channel,
                    "conversation-planned-resume",
                )
                .is_none()
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn live_observer_failure_cancels_exact_recovery_without_poisoning_frontier()
    -> Result<(), String> {
        use crate::chat_event_log::{ChatEventLog, ChatEventRetention};
        use crate::conversation_input::{
            ConversationInputAddress, ConversationInputPhase, ConversationInputService,
        };
        use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(temporary.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let address = ConversationInputAddress {
            workspace_id: "workspace-observer-failure".to_string(),
            conversation_id: "conversation-observer-failure".to_string(),
        };
        service
            .submit(
                address.clone(),
                "observer-failure-input".to_string(),
                "recover me".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let projection = service
            .dispatch_next(&address, "observer-failure-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "observer failure input was not dispatched".to_string())?;
        let attempt = super::channel_input_attempt(&projection)?;
        service
            .recovery_required(attempt.clone(), "injected observer failure".to_string())
            .await
            .map_err(|error| error.to_string())?;
        let observed_phase = Arc::new(std::sync::Mutex::new(Some(
            ConversationInputPhase::RecoveryRequired,
        )));
        let projector =
            super::channel_live_terminal_projector(service.clone(), attempt, observed_phase);
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                &address.workspace_id,
                ForegroundTurnSurface::Channel,
                &address.conversation_id,
                "observer-failure-turn",
            )
            .map_err(|error| error.to_string())?;
        control
            .supervise_input_lifecycle_scoped(
                &address.workspace_id,
                ForegroundTurnSurface::Channel,
                &address.conversation_id,
                "observer-failure-turn",
                async { Err("injected observer runtime failure".to_string()) },
                projector,
            )
            .map_err(|error| error.to_string())?;

        let settlement = lease
            .settle_after_observers(crate::chat_driver::TurnOutcome::Completed)
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            settlement.outcome,
            crate::chat_driver::TurnOutcome::Failed(_)
        ));
        let frontier = service
            .list(&address)
            .await
            .map_err(|error| error.to_string())?;
        assert!(frontier.items.is_empty());
        let terminal = service
            .submit(
                address,
                "observer-failure-input".to_string(),
                "recover me".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(terminal.phase, ConversationInputPhase::Cancelled);
        assert!(!terminal.drained);
        assert_eq!(
            terminal.reason.as_deref(),
            Some("injected observer failure")
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn continuation_uses_stable_root_for_control_and_current_turn_for_steer() {
        use crate::foreground_turn::{ForegroundTurnSnapshot, ForegroundTurnSurface};

        let snapshot = ForegroundTurnSnapshot {
            workspace_id: "workspace-a".to_string(),
            surface: ForegroundTurnSurface::Channel,
            conversation_id: "conversation-a".to_string(),
            root_turn_id: "root-turn".to_string(),
            active_turn_id: "continuation-turn".to_string(),
            run_id: Some("conversation-run".to_string()),
            cancellation_requested: false,
        };
        assert!(super::channel_snapshot_matches_root(&snapshot, "root-turn"));
        assert!(!super::channel_snapshot_matches_root(
            &snapshot,
            "continuation-turn"
        ));
        assert_eq!(super::channel_steer_target(&snapshot), "continuation-turn");
    }

    #[cfg(feature = "channels")]
    #[test]
    fn framework_owned_channel_root_is_resolved_without_local_pin() -> Result<(), String> {
        use crate::foreground_turn::{ForegroundTurnSnapshot, ForegroundTurnSurface};

        let unrelated = ForegroundTurnSnapshot {
            workspace_id: "workspace-a".to_string(),
            surface: ForegroundTurnSurface::Gui,
            conversation_id: "sender-conversation".to_string(),
            root_turn_id: "gui-root".to_string(),
            active_turn_id: "gui-root".to_string(),
            run_id: None,
            cancellation_requested: false,
        };
        let extraction = ForegroundTurnSnapshot {
            workspace_id: "workspace-a".to_string(),
            surface: ForegroundTurnSurface::Channel,
            conversation_id: "sender-conversation".to_string(),
            root_turn_id: "extract-root".to_string(),
            active_turn_id: "extract-root".to_string(),
            run_id: None,
            cancellation_requested: false,
        };
        let resolved = super::channel_snapshot_for_conversation(
            vec![unrelated, extraction],
            "sender-conversation",
        )?
        .ok_or_else(|| "framework-owned channel root was not resolved".to_string())?;
        assert_eq!(resolved.root_turn_id, "extract-root");
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn old_supervisor_cannot_clear_new_active_generation() {
        assert!(super::channel_active_generation_matches(
            Some("turn-2"),
            "turn-2"
        ));
        assert!(!super::channel_active_generation_matches(
            Some("turn-2"),
            "turn-1"
        ));
        assert!(!super::channel_active_generation_matches(None, "turn-1"));
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn foreground_continuation_steers_current_turn_and_root_cancel_settles()
    -> Result<(), String> {
        use crate::chat_driver::TurnOutcome;
        use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};
        use crate::tasks::task_runtime::{
            RunTurnBinding, RunTurnOrigin, TaskRuntimeStore, TurnVisibility,
        };

        let temporary =
            std::env::temp_dir().join(format!("eko-channel-foreground-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temporary).map_err(|error| error.to_string())?;
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        // Keep the original application owner alive through driver settlement.
        let mut lifecycle = crate::runtime::ApplicationLifecycleOwner::new(
            echo_agent::agent::CancellationToken::new(),
        );
        lifecycle.bind_task_runtime(store.clone());
        let workspace_id = store.active_workspace_id();
        let foreground_turns = ForegroundTurnControl::default();
        let lease = foreground_turns
            .begin_scoped(
                &workspace_id,
                ForegroundTurnSurface::Channel,
                "conversation-a",
                "root-turn",
            )
            .map_err(|error| error.to_string())?;
        let cancel = lease.cancellation_token();
        let llm = std::sync::Arc::new(ChannelCancellationBarrierLlmClient::new());
        let agent = echo_agent::agent::ReactAgentBuilder::new()
            .model("channel-cancellation-barrier")
            .llm_client(llm.clone())
            .build()
            .map(echo_agent::agent::AgentHandle::new)
            .map_err(|error| error.to_string())?;
        let agent_for_steer = agent.clone();
        let turn =
            crate::prepared_turn::PreparedUserTurn::build(crate::prepared_turn::UserTurnInput {
                text: "continue",
                attachments: &[],
                spill_dir: &temporary,
                conversation_id: Some("conversation-a"),
                turn_id: Some("root-turn"),
            })
            .map_err(|error| error.to_string())?;
        let resources = std::sync::Arc::new(crate::chat_resources::ChatResources {
            execution_scope: crate::workspace::WorkspaceExecutionScope::workspace(
                &crate::workspace::WorkspaceId::from_raw(workspace_id.clone()),
                &temporary,
            ),
            workspace_io_receipt: None,
            pool: None,
            store: Some(store),
            sink: std::sync::Arc::new(ChannelTestSink),
            webhook_emitter: None,
            conv_id: Some("conversation-a".to_string()),
            root_message_id: "root-turn".to_string(),
            attachments: Vec::new(),
            cancel,
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let binding = RunTurnBinding {
            domain_plugin: None,

            run_id: None,
            turn_id: "continuation-turn".to_string(),
            root_message_id: "root-turn".to_string(),
            origin: RunTurnOrigin::Continuation,
            transcript_visibility: TurnVisibility::Visible,
            expected_resume: None,
        };
        let driver = tokio::spawn(async move {
            crate::foreground_turn::drive_foreground_chat_turn(
                lease, &agent, &turn, resources, binding,
            )
            .await
        });
        if tokio::time::timeout(std::time::Duration::from_secs(2), llm.wait_started())
            .await
            .is_err()
        {
            if driver.is_finished() {
                let outcome = driver.await.map_err(|error| error.to_string())?;
                return Err(format!(
                    "channel driver ended before reaching model cancellation barrier: {outcome:?}"
                ));
            }
            return Err("channel model request did not reach cancellation barrier".to_string());
        }
        let snapshot_wait = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(snapshot) = foreground_turns.snapshot_scoped(
                    &workspace_id,
                    ForegroundTurnSurface::Channel,
                    "conversation-a",
                ) && snapshot.active_turn_id == "continuation-turn"
                {
                    return snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let snapshot = match snapshot_wait {
            Ok(snapshot) => snapshot,
            Err(_) if driver.is_finished() => {
                let outcome = driver.await.map_err(|error| error.to_string())?;
                return Err(format!(
                    "channel driver ended before publishing continuation identity: {outcome:?}"
                ));
            }
            Err(_) => {
                let observed = foreground_turns.snapshot_scoped(
                    &workspace_id,
                    ForegroundTurnSurface::Channel,
                    "conversation-a",
                );
                return Err(format!(
                    "continuation turn identity was not published; observed={observed:?}"
                ));
            }
        };
        assert!(super::channel_snapshot_matches_root(&snapshot, "root-turn"));
        let steer_target = super::channel_steer_target(&snapshot).to_string();
        let mut steer_receipt = agent_for_steer
            .steer_input_tracked(
                Some(&steer_target),
                echo_agent::prelude::Message::user("steer continuation".to_string()),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            steer_receipt.state(),
            echo_agent::agent::AgentSteerState::Accepted
                | echo_agent::agent::AgentSteerState::Drained
        ));
        assert_eq!(steer_receipt.turn_id(), "continuation-turn");
        let accepted_but_not_settled = foreground_turns
            .snapshot_scoped(
                &workspace_id,
                ForegroundTurnSurface::Channel,
                "conversation-a",
            )
            .ok_or_else(|| "accepted channel steer released the foreground slot".to_string())?;
        assert_eq!(accepted_but_not_settled.active_turn_id, "continuation-turn");
        assert!(!accepted_but_not_settled.cancellation_requested);

        let settlement = super::channel_cancel_root(
            &foreground_turns,
            &workspace_id,
            "conversation-a",
            "root-turn",
        )
        .await
        .map_err(|error| error.to_string())?;
        assert_eq!(settlement.turn_id, "root-turn");
        if !matches!(settlement.outcome, TurnOutcome::Cancelled) {
            return Err(format!(
                "channel root cancellation settled with {:?}",
                settlement.outcome
            ));
        }
        let settled = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            steer_receipt.wait_for_turn_settled(),
        )
        .await
        .map_err(|_| "tracked channel steer did not settle".to_string())?;
        assert!(matches!(
            settled,
            echo_agent::agent::AgentSteerState::TurnSettled { .. }
        ));
        let _driver_outcome = tokio::time::timeout(std::time::Duration::from_secs(2), driver)
            .await
            .map_err(|_| "cancelled channel driver did not settle".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(
            foreground_turns
                .snapshot_scoped(
                    &workspace_id,
                    ForegroundTurnSurface::Channel,
                    "conversation-a",
                )
                .is_none()
        );
        lifecycle
            .settle(crate::runtime::ApplicationLifecycleReason::Shutdown, None)
            .await
            .into_result()
            .map_err(|error| error.to_string())?;
        let _cleanup = std::fs::remove_dir_all(&temporary);
        Ok(())
    }

    #[tokio::test]
    async fn full_prompt_queue_recovers_without_poisoning_the_original_session()
    -> anyhow::Result<()> {
        use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
        let hitl = Arc::new(ChannelHumanLoopProvider::new());
        for attempt in 0..2 {
            let prompts = hitl.subscribe_prompts();
            let mut admitted = hitl.subscribe_prompts();
            let (render, mut output) = tokio::sync::mpsc::channel(1);
            render
                .send(ChannelRenderEvent::Prompt("occupied".into()))
                .await
                .map_err(|_| anyhow::anyhow!("fixture queue unavailable"))?;
            let original = hitl.clone();
            let waiting = tokio::spawn(async move {
                let drive = original.request(HumanLoopRequest::input("original input"));
                drive_with_channel_prompts(drive, prompts, render, original.as_ref()).await
            });
            tokio::time::timeout(std::time::Duration::from_secs(2), admitted.recv()).await??;
            for _ in 0..3 {
                tokio::task::yield_now().await;
            }
            assert!(!waiting.is_finished());
            assert!(
                matches!(output.recv().await, Some(ChannelRenderEvent::Prompt(text)) if text == "occupied")
            );
            let prompt = tokio::time::timeout(std::time::Duration::from_secs(2), output.recv())
                .await?
                .ok_or_else(|| anyhow::anyhow!("original prompt missing"))?;
            let ChannelRenderEvent::Prompt(prompt) = prompt else {
                return Err(anyhow::anyhow!("unexpected render event"));
            };
            let target = prompt
                .split_whitespace()
                .skip_while(|word| *word != "/reply")
                .nth(1)
                .ok_or_else(|| anyhow::anyhow!("original target missing"))?;
            assert!(matches!(
                hitl.try_resolve_message(&format!("/reply {target} answer-{attempt}")),
                ChannelHumanLoopResolution::Resolved(_)
            ));
            assert!(
                matches!(tokio::time::timeout(std::time::Duration::from_secs(2), waiting).await???,
                HumanLoopResponse::Text(text) if text == format!("answer-{attempt}"))
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn closed_prompt_observer_withdraws_current_im_only_and_gui_can_still_win()
    -> anyhow::Result<()> {
        use crate::hitl::{ChannelHumanLoopEntry, PairedHumanLoopProvider, PairedHumanLoopRequest};
        use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
        struct Gui {
            first_gate: Arc<tokio::sync::Notify>,
            second_gate: Arc<tokio::sync::Notify>,
            calls: std::sync::atomic::AtomicUsize,
            second_available: bool,
        }
        impl PairedHumanLoopProvider for Gui {
            fn request_paired(
                &self,
                paired: PairedHumanLoopRequest,
            ) -> futures::future::BoxFuture<'_, echo_agent::error::Result<HumanLoopResponse>>
            {
                Box::pin(async move {
                    paired.validate()?;
                    if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        self.first_gate.notified().await;
                        Ok(HumanLoopResponse::Text("GUI answer".into()))
                    } else if self.second_available {
                        self.second_gate.notified().await;
                        Ok(HumanLoopResponse::Text("GUI followup answer".into()))
                    } else {
                        Err(echo_agent::error::ReactError::Other(
                            "GUI observer unavailable".into(),
                        ))
                    }
                })
            }
        }
        for second_available in [false, true] {
            let hitl = Arc::new(ChannelHumanLoopProvider::new());
            let prompts = hitl.subscribe_prompts();
            let mut admitted = hitl.subscribe_prompts();
            let first_gate = Arc::new(tokio::sync::Notify::new());
            let second_gate = Arc::new(tokio::sync::Notify::new());
            let gui = Arc::new(Gui {
                first_gate: first_gate.clone(),
                second_gate: second_gate.clone(),
                calls: std::sync::atomic::AtomicUsize::new(0),
                second_available,
            });
            let factory: crate::hitl::ChannelGuiHitlFactory = Arc::new(move |_, _| Ok(gui.clone()));
            let entry = Arc::new(ChannelHumanLoopEntry::new(
                hitl.clone(),
                Arc::new(std::sync::Mutex::new(Some(factory))),
                Arc::new(|| Ok(Default::default())),
            ));
            let (render, output) = tokio::sync::mpsc::channel(1);
            drop(output);
            let original = hitl.clone();
            let waiting = tokio::spawn(async move {
                let drive = async move {
                    let first = entry
                        .request(HumanLoopRequest::input("paired input"))
                        .await?;
                    let mut request = HumanLoopRequest::input("second request in same drive");
                    request.timeout = Some(std::time::Duration::from_secs(60));
                    let second = entry.request(request).await?;
                    Ok::<_, echo_agent::error::ReactError>((first, second))
                };
                drive_with_channel_prompts(drive, prompts, render, original.as_ref()).await
            });
            let first_prompt =
                tokio::time::timeout(std::time::Duration::from_secs(2), admitted.recv()).await??;
            let old_target = first_prompt
                .split_whitespace()
                .skip_while(|word| *word != "/reply")
                .nth(1)
                .ok_or_else(|| anyhow::anyhow!("first target missing"))?
                .to_string();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if matches!(
                        hitl.try_resolve_message("ordinary text"),
                        ChannelHumanLoopResolution::NoPending
                    ) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await?;
            first_gate.notify_one();
            let second_prompt =
                tokio::time::timeout(std::time::Duration::from_secs(2), admitted.recv()).await??;
            assert!(!second_prompt.contains(&old_target));
            assert!(!matches!(
                hitl.try_resolve_message(&format!("/reply {old_target} late")),
                ChannelHumanLoopResolution::Resolved(_)
            ));
            // Require the actual same-drive followup to withdraw IM before
            // allowing its GUI response. Neither path waits the60s deadline.
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if matches!(
                        hitl.try_resolve_message("ordinary text"),
                        ChannelHumanLoopResolution::NoPending
                    ) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await?;
            second_gate.notify_one();
            let (first, second) =
                tokio::time::timeout(std::time::Duration::from_secs(2), waiting).await???;
            assert!(matches!(first, HumanLoopResponse::Text(text) if text == "GUI answer"));
            if second_available {
                assert!(
                    matches!(second, HumanLoopResponse::Text(text) if text == "GUI followup answer")
                );
            } else {
                assert!(matches!(second, HumanLoopResponse::Rejected { .. }));
            }
            // Same original provider, actual fresh independent renderer/helper.
            let prompts = hitl.subscribe_prompts();
            let (render, mut output) = tokio::sync::mpsc::channel(1);
            let original = hitl.clone();
            let next = tokio::spawn(async move {
                drive_with_channel_prompts(
                    original.request(HumanLoopRequest::input("next turn")),
                    prompts,
                    render,
                    original.as_ref(),
                )
                .await
            });
            let prompt = tokio::time::timeout(std::time::Duration::from_secs(2), output.recv())
                .await?
                .ok_or_else(|| anyhow::anyhow!("fresh renderer prompt missing"))?;
            let ChannelRenderEvent::Prompt(prompt) = prompt else {
                return Err(anyhow::anyhow!("unexpected fresh renderer event"));
            };
            let target = prompt
                .split_whitespace()
                .skip_while(|word| *word != "/reply")
                .nth(1)
                .ok_or_else(|| anyhow::anyhow!("next target missing"))?;
            assert!(matches!(
                hitl.try_resolve_message(&format!("/reply {target} next")),
                ChannelHumanLoopResolution::Resolved(_)
            ));
            assert!(
                matches!(tokio::time::timeout(std::time::Duration::from_secs(2), next).await???, HumanLoopResponse::Text(text) if text == "next")
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn original_pump_drain_survives_waiter_drop_and_keeps_failure_receipt()
    -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let coordinator = Arc::new(super::ChannelSessionCoordinator::new());
        let finished = Arc::new(AtomicUsize::new(0));
        let (release, held) = tokio::sync::oneshot::channel();
        let actor_finished = finished.clone();
        {
            let tasks = coordinator
                .input_pump_tasks
                .lock()
                .map_err(|_| anyhow::anyhow!("pump owner lock"))?;
            let mut original = tasks
                .tasks
                .lock()
                .map_err(|_| anyhow::anyhow!("original pump set unavailable"))?;
            original.spawn(async { Err("original pump failure".to_string()) });
            original.spawn(async move {
                held.await.map_err(|error| error.to_string())?;
                actor_finished.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        }
        let observer = coordinator.clone();
        let first_waiter = tokio::spawn(async move { observer.join_input_pumps().await });
        // Establish that the drain has actually consumed the first Err while
        // the second original task remains held. Task startup alone is not proof.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let consumed = {
                    let tasks = coordinator
                        .input_pump_tasks
                        .lock()
                        .map_err(|_| anyhow::anyhow!("pump owner lock"))?;
                    let failed = !tasks
                        .joined_failures
                        .lock()
                        .map_err(|_| anyhow::anyhow!("failure receipts unavailable"))?
                        .is_empty();
                    tasks.settlement.is_some() && failed
                };
                if consumed {
                    break Ok::<_, anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        assert!(coordinator.reopen_input_pump_admission().is_err());
        let observer = coordinator.clone();
        let second_waiter = tokio::spawn(async move { observer.join_input_pumps().await });
        tokio::task::yield_now().await;
        first_waiter.abort();
        assert!(first_waiter.await.is_err());
        assert!(!second_waiter.is_finished());
        assert_eq!(finished.load(Ordering::SeqCst), 0);
        release
            .send(())
            .map_err(|_| anyhow::anyhow!("original task was aborted by waiter drop"))?;
        let first =
            tokio::time::timeout(std::time::Duration::from_secs(2), second_waiter).await??;
        let second = coordinator.join_input_pumps().await;
        assert_eq!(first, Err("original pump failure".to_string()));
        assert_eq!(second, first);
        assert_eq!(
            coordinator
                .observe_input_pump_settlement()
                .await
                .map_err(anyhow::Error::msg)?
                .failures,
            vec!["original pump failure".to_string()]
        );
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        // The completed diagnostic is an error, but every original task really
        // joined. Only now may explicit reconnect reopen the same coordinator.
        coordinator
            .reopen_input_pump_admission()
            .map_err(anyhow::Error::msg)?;
        assert!(coordinator.join_input_pumps().await.is_ok());
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn actual_cancel_command_cannot_reject_successor_with_reused_request_id()
    -> anyhow::Result<()> {
        use crate::hitl::ChannelHumanLoopResolution;
        use echo_agent::channels::{
            ChatType, InboundMessage, MessageHandler, SessionConfig, SessionHandler,
        };
        use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};

        let temporary = tempfile::tempdir()?;
        let state = Arc::new(crate::state::AppState::from_shared(
            channel_test_agent(std::time::Duration::ZERO).map_err(anyhow::Error::msg)?,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                temporary.path().join("mcp.json"),
                echo_agent::mcp::McpConfigFile::default(),
            )),
            crate::product_data_io::ProductDataIoService::new(),
        )?);
        let coordinator = Arc::new(super::ChannelSessionCoordinator::new());
        let captured = Arc::new(std::sync::Mutex::new(None));
        let factory_captured = captured.clone();
        let factory_state = state.clone();
        let factory_coordinator = coordinator.clone();
        let foreground = crate::foreground_turn::ForegroundTurnControl::default();
        let factory_foreground = foreground.clone();
        let sessions = SessionHandler::new(
            SessionConfig::default(),
            move |instance: &echo_agent::channels::ChannelSessionInstance| {
                let handler = Arc::new(super::AppChannelMessageHandler::new(
                    factory_state.clone(),
                    Arc::new(crate::webhook::WebhookEmitter::new()),
                    factory_foreground.clone(),
                    instance.clone(),
                    factory_coordinator.clone(),
                ));
                *factory_captured
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(handler);
                Box::new(SessionIdentityProbe) as Box<dyn MessageHandler>
            },
        );
        sessions
            .handle(InboundMessage::new(
                "test-channel",
                "sender",
                "chat",
                ChatType::Direct,
                "fixture",
                "fixture",
            ))
            .await?;
        let handler = captured
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| anyhow::anyhow!("actual channel handler was not constructed"))?;
        let conv =
            super::AppChannelMessageHandler::conversation_id("test-channel", "chat", "sender");
        let agent_conv = super::AppChannelMessageHandler::agent_conversation_id(
            &conv,
            &handler.session_instance.incarnation_id(),
        );
        let surface = super::ChannelSessionCoordinator::surface_id(&handler.session_instance);
        let runtime = state.current_control_runtime().await?;
        let lease = foreground.begin_scoped(
            runtime.execution_scope().workspace_id(),
            crate::foreground_turn::ForegroundTurnSurface::Channel,
            &conv,
            "original-root",
        )?;
        handler
            .publish_active_turn(&surface, &conv, &agent_conv, &runtime, "original-root")
            .map_err(anyhow::Error::msg)?;
        lease.retain(super::ChannelActiveTurnOwner::new(
            coordinator.active_turns.clone(),
            surface.clone(),
            "original-root".to_string(),
        ));
        let mut prompts = handler.hitl.subscribe_prompts();
        let mut request = HumanLoopRequest::input("中文\n🙂");
        request.request_id = Some("reusable-id".to_string());
        request.timeout = Some(std::time::Duration::from_secs(2));
        let first_provider = handler.hitl.clone();
        let first_request = request.clone();
        let first = tokio::spawn(async move { first_provider.request(first_request).await });
        let _first_prompt =
            tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv()).await??;
        first.abort();
        let _cancelled = first.await;
        let next_provider = handler.hitl.clone();
        let next = tokio::spawn(async move { next_provider.request(request).await });
        let prompt =
            tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv()).await??;
        let target = prompt
            .split("Use /reply ")
            .nth(1)
            .and_then(|suffix| suffix.split_whitespace().next())
            .ok_or_else(|| anyhow::anyhow!("successor target missing"))?;
        let response = handler
            .control_command_response(
                "/cancel",
                &conv,
                &agent_conv,
                "fixture-cache",
                &surface,
                None,
            )
            .await
            .ok_or_else(|| anyhow::anyhow!("actual cancel command was not handled"))?;
        assert!(response.contains(target));
        assert!(!next.is_finished());
        assert!(!lease.cancellation_token().is_cancelled());
        assert!(matches!(
            handler
                .hitl
                .try_resolve_message(&format!("/reply {target} 中文\n🙂")),
            ChannelHumanLoopResolution::Resolved(_)
        ));
        assert!(matches!(next.await??, HumanLoopResponse::Text(text) if text == "中文\n🙂"));
        lease
            .settle_after_observers(crate::chat_driver::TurnOutcome::Completed)
            .await?;
        assert!(
            handler
                .hitl
                .request(HumanLoopRequest::input("owner missing"))
                .await
                .is_err()
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn dropped_old_owner_keeps_replacement_in_real_active_map() -> Result<(), String> {
        let temporary =
            std::env::temp_dir().join(format!("eko-channel-owner-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temporary).map_err(|error| error.to_string())?;
        let agent = channel_test_agent(std::time::Duration::ZERO)?;
        let mcp_runtime =
            std::sync::Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                temporary.join("mcp.json"),
                echo_agent::mcp::McpConfigFile::default(),
            ));
        let state = crate::state::AppState::from_shared(
            agent,
            None,
            std::sync::Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp_runtime,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?;
        let runtime = state
            .current_control_runtime()
            .await
            .map_err(|error| error.to_string())?;
        let active_turns: super::ChannelActiveTurnMap =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let surface =
            super::AppChannelMessageHandler::active_surface_identity("qq", "chat-a", "sender-a");
        {
            let mut active = active_turns
                .lock()
                .map_err(|_| "active turn map lock poisoned".to_string())?;
            active.insert(
                surface.clone(),
                super::ChannelActiveTurn {
                    runtime: runtime.clone(),
                    agent_conversation_id: "agent-conversation-a".to_string(),
                    conversation_id: "conversation-a".to_string(),
                    turn_id: "turn-1".to_string(),
                },
            );
        }
        let old_owner = super::ChannelActiveTurnOwner::new(
            std::sync::Arc::clone(&active_turns),
            surface.clone(),
            "turn-1".to_string(),
        );
        {
            let mut active = active_turns
                .lock()
                .map_err(|_| "active turn map lock poisoned".to_string())?;
            active.insert(
                surface.clone(),
                super::ChannelActiveTurn {
                    runtime,
                    agent_conversation_id: "agent-conversation-a".to_string(),
                    conversation_id: "conversation-a".to_string(),
                    turn_id: "turn-2".to_string(),
                },
            );
        }
        drop(old_owner);
        let retained_turn = active_turns
            .lock()
            .map_err(|_| "active turn map lock poisoned".to_string())?
            .get(&surface)
            .map(|active| active.turn_id.clone());
        assert_eq!(retained_turn.as_deref(), Some("turn-2"));
        let _cleanup = std::fs::remove_dir_all(&temporary);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn management_commands_are_classified_exactly() {
        assert!(super::is_agent_management_command("/trace run-1"));
        for command in ["/analysis", "/papers"] {
            assert!(super::is_agent_management_command(command));
        }
        assert!(!super::is_agent_management_command(" /skills list "));
        assert!(!super::is_agent_management_command("/browser status"));
        assert!(!super::is_agent_management_command("/stop"));
        assert!(!super::is_agent_management_command("/traceable"));
    }

    #[cfg(feature = "channels")]
    #[test]
    fn typed_extension_parser_claims_all_families_before_the_model() -> Result<(), String> {
        assert!(
            super::parse_channel_extension_input(
                "/lsp status",
                "retired-request",
                "retired-operation"
            )
            .is_err_and(|message| message.contains("retired"))
        );

        for command in [
            "/skills list",
            "/plugins list",
            "/mcp list",
            "/hooks list",
            "/browser status",
        ] {
            let parsed = super::parse_channel_extension_input(command, "request-1", "operation-1")?
                .ok_or_else(|| format!("{command} was not claimed by the Extension parser"))?;
            if !matches!(parsed, super::ChannelExtensionInput::Request(_)) {
                return Err(format!("{command} did not produce a typed request"));
            }
        }
        let invalid =
            super::parse_channel_extension_input("/browser unknown", "request-2", "operation-2")?
                .ok_or_else(|| "invalid Browser command was sent to the model".to_string())?;
        assert!(matches!(
            invalid,
            super::ChannelExtensionInput::ParseFailure {
                kind: crate::extension_commands::ExtensionKind::Browser,
                ..
            }
        ));
        assert!(
            super::parse_channel_extension_input("ordinary prompt", "request-3", "operation-3")?
                .is_none()
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn non_global_extension_scope_uses_product_data_generation() -> Result<(), String> {
        let temporary = std::env::temp_dir().join(format!(
            "eko-channel-extension-scope-{}",
            uuid::Uuid::new_v4()
        ));
        let workspace_root = temporary.join("workspace");
        std::fs::create_dir_all(&workspace_root).map_err(|error| error.to_string())?;
        let registry = crate::workspace::registry::WorkspaceRegistry::with_base_dir(
            temporary.join("registry"),
        )
        .map_err(|error| error.to_string())?;
        let workspace = registry
            .create_at(
                "channel-extension-scope",
                crate::workspace::WorkspaceKind::General,
                workspace_root,
            )
            .map_err(|error| error.to_string())?;
        let product_data_generation = workspace.opaque_product_data_generation();
        let host_generation = workspace
            .created_at
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);

        let scope = super::channel_extension_scope_from_product_data(
            workspace.id.as_str(),
            product_data_generation.clone(),
            "sender-a",
            "incarnation-a",
        )?;

        assert_ne!(workspace.id.as_str(), "global");
        assert_eq!(scope.workspace_id, workspace.id.as_str());
        assert_eq!(scope.workspace_generation, product_data_generation);
        assert_ne!(scope.workspace_generation, host_generation);
        let decoded: (String, String, u64) =
            serde_json::from_str(&scope.workspace_generation).map_err(|error| error.to_string())?;
        assert_eq!(decoded.0, workspace.id.as_str());
        let _cleanup = std::fs::remove_dir_all(&temporary);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn shared_developer_catalog_is_parsed_before_chat() -> Result<(), String> {
        for descriptor in crate::developer_commands::DeveloperCommandRegistry::commands() {
            let parsed = super::parse_developer_command(&format!("/{} status", descriptor.name))?
                .ok_or_else(|| format!("/{} was not recognized", descriptor.name))?;
            assert_eq!(parsed.0, descriptor.name);
            assert_eq!(parsed.1, vec!["status"]);
        }
        let alias = super::parse_developer_command("/term list")?
            .ok_or_else(|| "/term was not recognized".to_string())?;
        assert_eq!(alias.0, "terminal");
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn tasks_immediate_response_redacts_and_bounds_multi_megabyte_text() -> Result<(), String>
    {
        use echo_agent::channels::{ChatType, InboundMessage};
        use futures::StreamExt;

        let secret = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
        let message = InboundMessage::new(
            "qq",
            "user",
            "conversation",
            ChatType::Direct,
            "/tasks",
            "message-1",
        );
        let huge = format!("Task list token={secret}\n{}", "任务🙂".repeat(500_000));
        let mut stream = super::channel_immediate_response(&message, huge).into_stream();
        let mut texts = Vec::new();
        while let Some(item) = stream.next().await {
            texts.push(item.map_err(|error| error.to_string())?.text);
        }
        let joined = texts.join("\n");
        assert!(!joined.contains(secret));
        assert!(joined.contains("[REDACTED]"));
        assert!(texts.iter().all(|text| text.len() <= 1_800));
        assert!(texts.len() <= super::CHANNEL_OUTBOUND_TOTAL_MESSAGES);
        Ok(())
    }

    #[cfg(all(feature = "channels", unix))]
    #[tokio::test]
    async fn terminal_stream_keeps_fast_output_from_pre_dispatch_subscription() -> Result<(), String>
    {
        use echo_agent::channels::{ChatType, InboundMessage};
        use futures::StreamExt;

        let terminal = crate::terminal::TerminalService::new();
        let receiver = terminal.subscribe();
        terminal
            .create_with_shell_for_test(
                "channel-fast".to_string(),
                None,
                24,
                80,
                "/bin/sh".to_string(),
            )
            .await?;
        terminal
            .write("channel-fast", b"printf channel-fast-output; exit\n")
            .await?;
        let message = InboundMessage::new(
            "qq",
            "user",
            "conversation",
            ChatType::Direct,
            "/terminal create channel-fast",
            "message-1",
        );
        let mut stream = super::channel_terminal_stream(
            &message,
            "created".to_string(),
            receiver,
            "channel-fast".to_string(),
        );
        let texts = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut texts = Vec::new();
            while let Some(message) = stream.next().await {
                let message = message.map_err(|error| error.to_string())?;
                let exited = message.text.contains("exited:");
                texts.push(message.text);
                if exited {
                    break;
                }
            }
            Ok::<_, String>(texts)
        })
        .await
        .map_err(|_| "channel terminal stream did not observe exit".to_string())??;
        assert!(
            texts
                .iter()
                .any(|text| text.contains("channel-fast-output"))
        );
        if !texts.iter().any(|text| text.contains("exited:")) {
            return Err(format!(
                "channel terminal stream ended without exit receipt: {texts:?}"
            ));
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn terminal_stream_accepts_bursty_pty_chunks_until_total_budget() -> Result<(), String> {
        use crate::terminal::{TerminalEvent, TerminalExitReason};
        use echo_agent::channels::{ChatType, InboundMessage};
        use futures::StreamExt;

        let (sender, receiver) = tokio::sync::broadcast::channel(128);
        for _ in 0..64 {
            sender
                .send(TerminalEvent::Output {
                    id: "bursty-terminal".to_string(),
                    bytes: vec![b'x'],
                })
                .map_err(|error| error.to_string())?;
        }
        sender
            .send(TerminalEvent::Exited {
                id: "bursty-terminal".to_string(),
                reason: TerminalExitReason::ProcessExited,
                exit_code: 0,
                signal: None,
            })
            .map_err(|error| error.to_string())?;
        let message = InboundMessage::new(
            "qq",
            "user",
            "conversation",
            ChatType::Direct,
            "/terminal create bursty-terminal",
            "message-1",
        );
        let mut stream = super::channel_terminal_stream(
            &message,
            "created".to_string(),
            receiver,
            "bursty-terminal".to_string(),
        );
        let mut texts = Vec::new();
        while let Some(item) = stream.next().await {
            texts.push(item.map_err(|error| error.to_string())?.text);
        }
        let joined = texts.join("");
        assert!(joined.contains(&"x".repeat(64)));
        assert!(joined.contains("exited:"));
        assert!(!joined.contains("forwarding detached"));
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn terminal_stream_preserves_utf8_split_at_every_byte() -> Result<(), String> {
        use crate::terminal::{TerminalEvent, TerminalExitReason};
        use echo_agent::channels::{ChatType, InboundMessage};
        use futures::StreamExt;

        let (sender, receiver) = tokio::sync::broadcast::channel(32);
        let expected = "中文🙂";
        for byte in expected.as_bytes() {
            sender
                .send(TerminalEvent::Output {
                    id: "utf8-terminal".to_string(),
                    bytes: vec![*byte],
                })
                .map_err(|error| error.to_string())?;
        }
        sender
            .send(TerminalEvent::Exited {
                id: "utf8-terminal".to_string(),
                reason: TerminalExitReason::ProcessExited,
                exit_code: 0,
                signal: None,
            })
            .map_err(|error| error.to_string())?;
        let message = InboundMessage::new(
            "qq",
            "user",
            "conversation",
            ChatType::Direct,
            "/terminal create utf8-terminal",
            "message-1",
        );
        let mut stream = super::channel_terminal_stream(
            &message,
            "created".to_string(),
            receiver,
            "utf8-terminal".to_string(),
        );
        let mut joined = String::new();
        while let Some(item) = stream.next().await {
            joined.push_str(&item.map_err(|error| error.to_string())?.text);
        }
        assert!(joined.contains(expected));
        assert!(!joined.contains('\u{fffd}'));
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn terminal_stream_strips_split_csi_and_osc_at_every_byte() -> Result<(), String> {
        use crate::terminal::{TerminalEvent, TerminalExitReason};
        use echo_agent::channels::{ChatType, InboundMessage};
        use futures::StreamExt;

        let (sender, receiver) = tokio::sync::broadcast::channel(32);
        let encoded = b"\x1b[31m\xe4\xb8\xad\xf0\x9f\x99\x82\x1b[0m|\x1b]x\x07\xe6\x96\x87";
        for byte in encoded {
            sender
                .send(TerminalEvent::Output {
                    id: "ansi-terminal".to_string(),
                    bytes: vec![*byte],
                })
                .map_err(|error| error.to_string())?;
        }
        sender
            .send(TerminalEvent::Exited {
                id: "ansi-terminal".to_string(),
                reason: TerminalExitReason::ProcessExited,
                exit_code: 0,
                signal: None,
            })
            .map_err(|error| error.to_string())?;
        let message = InboundMessage::new(
            "qq",
            "user",
            "conversation",
            ChatType::Direct,
            "/terminal create ansi-terminal",
            "message-1",
        );
        let mut stream = super::channel_terminal_stream(
            &message,
            "created".to_string(),
            receiver,
            "ansi-terminal".to_string(),
        );
        let mut joined = String::new();
        while let Some(item) = stream.next().await {
            joined.push_str(&item.map_err(|error| error.to_string())?.text);
        }
        assert!(joined.contains("中🙂|文"));
        assert!(!joined.contains('\u{1b}'));
        assert!(!joined.contains("31m"));
        assert!(!joined.contains("]x"));
        Ok(())
    }

    #[cfg(all(feature = "channels", unix))]
    #[tokio::test]
    async fn terminal_stream_detaches_secret_multi_megabyte_process_at_budget() -> Result<(), String>
    {
        use echo_agent::channels::{ChatType, InboundMessage};
        use futures::StreamExt;

        let secret = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
        let terminal = crate::terminal::TerminalService::new();
        let receiver = terminal.subscribe();
        terminal
            .create("channel-budget".to_string(), None, 24, 80)
            .await?;
        terminal
            .write(
                "channel-budget",
                format!(
                    "printf 'Bearer {secret} '; head -c 2000000 /dev/zero | tr '\\000' x; sleep 30\r"
                )
                .as_bytes(),
            )
            .await?;
        let message = InboundMessage::new(
            "qq",
            "user",
            "conversation",
            ChatType::Direct,
            "/terminal create channel-budget",
            "message-1",
        );
        let mut stream = super::channel_terminal_stream(
            &message,
            "created".to_string(),
            receiver,
            "channel-budget".to_string(),
        );
        let texts = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut texts = Vec::new();
            while let Some(item) = stream.next().await {
                texts.push(item.map_err(|error| error.to_string())?.text);
            }
            Ok::<_, String>(texts)
        })
        .await
        .map_err(|_| "budgeted channel terminal did not settle".to_string())??;
        let joined = texts.join("\n");
        assert!(!joined.contains(secret));
        assert!(!joined.contains("Bearer ghp_"));
        assert!(
            joined.contains("forwarding detached") || joined.contains("exited:"),
            "terminal had no reserved settlement: {joined}"
        );
        if joined.contains("forwarding detached") {
            assert!(terminal.contains("channel-budget"));
        }
        assert!(texts.iter().all(|text| text.len() <= 1_800));
        assert!(texts.len() <= super::CHANNEL_OUTBOUND_TOTAL_MESSAGES);
        let _closed = terminal.close("channel-budget").await?;
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn downstream_drop_cancels_same_token_without_releasing_registry()
    -> Result<(), crate::foreground_turn::ForegroundTurnError> {
        use crate::chat_driver::TurnOutcome;
        use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Channel, "channel:test", "turn-1")?;
        let token = lease.cancellation_token();
        let guard = super::ChannelStreamDropGuard(token.clone());
        drop(guard);
        assert!(token.is_cancelled());
        assert!(
            control
                .snapshot(ForegroundTurnSurface::Channel, "channel:test")
                .is_some(),
            "stream disconnect requests cancellation but settlement owns registry release"
        );
        lease.settle_after_observers(TurnOutcome::Cancelled).await?;
        assert!(
            control
                .snapshot(ForegroundTurnSurface::Channel, "channel:test")
                .is_none()
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn bounded_surface_sink_cancels_when_consumer_does_not_poll() {
        use crate::chat_driver::{ChatDriverEvent, ChatSink};

        let (tx, rx) = tokio::sync::mpsc::channel(super::CHANNEL_EVENT_QUEUE_CAPACITY);
        let cancellation = echo_agent::agent::CancellationToken::new();
        let sink = super::ChannelSurfaceSink::new(tx, cancellation.clone());
        for index in 0..super::CHANNEL_EVENT_QUEUE_CAPACITY {
            assert!(sink.on_event(ChatDriverEvent::TurnStatus {
                status: format!("running-{index}"),
            }));
        }
        assert!(!sink.on_event(ChatDriverEvent::TurnStatus {
            status: "queue-full".to_string(),
        }));
        assert!(cancellation.is_cancelled());
        assert_eq!(rx.len(), super::CHANNEL_EVENT_QUEUE_CAPACITY);
    }

    #[cfg(feature = "channels")]
    #[test]
    fn native_surface_sink_keeps_large_utf8_block_and_rejects_oversized_event() -> Result<(), String>
    {
        use crate::chat_driver::{ChatDriverEvent, ChatSink};
        let (tx, mut rx) = tokio::sync::mpsc::channel(super::CHANNEL_EVENT_QUEUE_CAPACITY);
        let cancellation = echo_agent::agent::CancellationToken::new();
        let sink = super::ChannelSurfaceSink::new(tx, cancellation.clone());
        let original = native_commentary_event(1, "中文🙂".repeat(25_000))
            .map_err(|error| error.to_string())?;
        assert!(sink.on_event(original.clone()));
        let super::ChannelRenderEvent::Driver(delivered) =
            rx.try_recv().map_err(|error| error.to_string())?
        else {
            return Err("native block lost its original envelope".to_string());
        };
        assert_eq!(
            serde_json::to_value(delivered).map_err(|error| error.to_string())?,
            serde_json::to_value(original).map_err(|error| error.to_string())?
        );
        assert!(!cancellation.is_cancelled());
        assert!(rx.try_recv().is_err());
        let oversized =
            native_commentary_event(2, "中".repeat(super::outbound::CHANNEL_TURN_EVENT_BYTES))
                .map_err(|error| error.to_string())?;
        assert!(matches!(&oversized, ChatDriverEvent::Agent(envelope)
            if matches!(&envelope.payload, echo_agent::agent::AgentEvent::AssistantMessage { .. })));
        assert!(!sink.on_event(oversized));
        assert!(cancellation.is_cancelled());
        assert!(
            rx.try_recv().is_err(),
            "拒绝超限原事件，不能匿名截断或造新envelope"
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn channel_observation_preserves_identity_without_terminal() -> Result<(), String> {
        use crate::chat_driver::ChatTurnError;
        use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};
        use echo_agent::error::ReactError;
        use futures::StreamExt;

        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let state = crate::state::AppState::from_shared(
            channel_test_agent(std::time::Duration::ZERO)?,
            None,
            Arc::new(crate::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            Arc::new(crate::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                temporary.path().join("mcp.json"),
                echo_agent::mcp::McpConfigFile::default(),
            )),
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(|error| error.to_string())?;
        let control = ForegroundTurnControl::default();
        let lease = control
            .begin_scoped(
                "workspace-observed",
                ForegroundTurnSurface::Channel,
                "conversation-observed",
                "turn-observed",
            )
            .map_err(|error| error.to_string())?;
        let waiter = control
            .settlement_waiter_scoped(
                "workspace-observed",
                ForegroundTurnSurface::Channel,
                "conversation-observed",
                "turn-observed",
            )
            .map_err(|error| error.to_string())?;
        let coordinator = super::ChannelSessionCoordinator::new();
        let adapter = Arc::new(super::ChannelInputPumpAdapter {
            retirement: None,
            weixin_identity: None,
            session_instance: None,
            reply_slot: None,
            service_cancel: tokio_util::sync::CancellationToken::new(),
            weixin_delivery: None,
            bound_control_runtime: None,
            active_turns: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            surface_id: super::ChannelSurfaceIdentity {
                channel_id: "test".to_string(),
                chat_id: "test".to_string(),
                sender_id: "test".to_string(),
            },
            app_state: Arc::new(state),
            foreground_turns: control.clone(),
            address: crate::conversation_input::ConversationInputAddress {
                workspace_id: "workspace-observed".to_string(),
                conversation_id: "conversation-observed".to_string(),
            },
            agent_conversation_id: "agent-conversation-observed".to_string(),
            cache_id: "observed-cache".to_string(),
            hitl: Arc::new(super::ChannelHumanLoopProvider::new()),
            human_loop_provider: Arc::new(super::ChannelHumanLoopProvider::new()),
        });
        let (_driver_tx, driver_rx) = tokio::sync::mpsc::channel(1);
        let (_prompt_tx, prompt_rx) = tokio::sync::broadcast::channel::<String>(1);
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        coordinator.start_post_settlement_input_task(
            waiter,
            terminal_tx,
            Arc::new(super::input_pump::ChannelInputPumpSlot::default()),
            adapter,
        )?;
        lease.retain_observation(ChatTurnError::Observation {
            run_id: "channel-original-run".to_string(),
            execution_id: Some("channel-original-execution".to_string()),
        });
        let notifier = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            coordinator.join_input_pumps(),
        )
        .await
        .map_err(|_| "channel observation notifier did not return".to_string())?;
        drop(lease);
        let mut stream = super::channel_render_event_stream(
            driver_rx,
            prompt_rx,
            terminal_rx,
            super::ChannelStreamDropGuard(echo_agent::agent::CancellationToken::new()),
        );
        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .map_err(|_| "channel observation did not return".to_string())?
            .ok_or_else(|| "channel ended without observation".to_string())?;
        assert!(matches!(
            observed,
            Err(ReactError::ExecutionNotSettled { run_id, execution_id })
                if run_id == "channel-original-run"
                    && execution_id.as_deref() == Some("channel-original-execution")
        ));
        assert!(stream.next().await.is_none());
        assert!(
            notifier.is_ok(),
            "observation is not notifier failure: {notifier:?}"
        );
        assert!(
            control
                .snapshot_scoped(
                    "workspace-observed",
                    ForegroundTurnSurface::Channel,
                    "conversation-observed"
                )
                .is_some()
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn terminal_drains_accepted_final_answer_before_publication() -> Result<(), String> {
        use crate::chat_driver::{ChatDriverEvent, TurnOutcome};
        use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity};
        use futures::StreamExt;

        // Fill the only ordinary data slot; terminal still arrives through its
        // independent oneshot receipt and cannot be displaced by this event.
        let (driver_tx, driver_rx) = tokio::sync::mpsc::channel(1);
        let identity = EventIdentity::new("channel-driver", "channel-conversation")
            .map_err(|error| error.to_string())?;
        let final_answer = EventEnvelope::new(
            &identity,
            1,
            None,
            AgentEvent::FinalAnswer("complete answer".to_string()),
        )
        .map_err(|error| error.to_string())?;
        driver_tx
            .try_send(super::ChannelRenderEvent::Driver(ChatDriverEvent::Agent(
                Box::new(final_answer),
            )))
            .map_err(|_| "channel driver receiver closed".to_string())?;
        let (_prompt_tx, prompt_rx) = tokio::sync::broadcast::channel::<String>(1);
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        terminal_tx
            .send(Ok(TurnOutcome::Completed))
            .map_err(|_| "channel terminal receiver closed".to_string())?;
        drop(driver_tx);
        let cancellation = echo_agent::agent::CancellationToken::new();
        let mut stream = super::channel_render_event_stream(
            driver_rx,
            prompt_rx,
            terminal_rx,
            super::ChannelStreamDropGuard(cancellation.clone()),
        );

        let first = stream
            .next()
            .await
            .ok_or_else(|| "missing queued final answer".to_string())?
            .map_err(|error| error.to_string())?;
        let final_answer_first = matches!(
            first,
            super::ChannelRenderEvent::Driver(ChatDriverEvent::Agent(envelope))
                if matches!(envelope.payload, AgentEvent::FinalAnswer(ref answer) if answer == "complete answer")
        );
        if !final_answer_first {
            return Err("terminal was published before the queued final answer".to_string());
        }
        let second = stream
            .next()
            .await
            .ok_or_else(|| "missing terminal receipt".to_string())?
            .map_err(|error| error.to_string())?;
        if !matches!(
            second,
            super::ChannelRenderEvent::Terminal(TurnOutcome::Completed)
        ) {
            return Err("queued driver event was not followed by terminal receipt".to_string());
        }
        if stream.next().await.is_some() {
            return Err("channel stream continued after terminal receipt".to_string());
        }
        assert!(cancellation.is_cancelled());
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn observation_drains_accepted_body_before_error_without_waiting_for_owner_eof()
    -> Result<(), String> {
        use crate::chat_driver::{ChatDriverEvent, ChatTurnError};
        use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity};
        use futures::StreamExt;

        // 原owner仍持有sender：非终态观察不能等待其EOF，也不能丢弃sink已接受的正文。
        for terminal_error in [
            Some(ChatTurnError::Observation {
                run_id: "original-run".into(),
                execution_id: Some("original-execution".into()),
            }),
            Some(ChatTurnError::Error("original renderer failure".into())),
            None,
        ] {
            for _ in 0..32 {
                let (driver_tx, driver_rx) = tokio::sync::mpsc::channel(1);
                let identity = EventIdentity::new("observation-stream", "observation-turn")
                    .map_err(|error| error.to_string())?;
                let body = EventEnvelope::new(
                    &identity,
                    1,
                    None,
                    AgentEvent::FinalAnswer("accepted body".to_string()),
                )
                .map_err(|error| error.to_string())?;
                driver_tx
                    .try_send(super::ChannelRenderEvent::Driver(ChatDriverEvent::Agent(
                        Box::new(body),
                    )))
                    .map_err(|_| "observation driver receiver closed".to_string())?;
                let (prompt_tx, prompt_rx) = tokio::sync::broadcast::channel::<String>(1);
                drop(prompt_tx);
                let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
                match terminal_error.clone() {
                    Some(error) => terminal_tx
                        .send(Err(error))
                        .map_err(|_| "observation receipt receiver closed".to_string())?,
                    None => drop(terminal_tx),
                }
                let mut stream = super::channel_render_event_stream(
                    driver_rx,
                    prompt_rx,
                    terminal_rx,
                    super::ChannelStreamDropGuard(echo_agent::agent::CancellationToken::new()),
                );
                let first = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
                    .await
                    .map_err(|_| "observation waited for live producer EOF".to_string())?
                    .ok_or_else(|| "missing accepted body".to_string())?
                    .map_err(|error| format!("observation overtook accepted body: {error}"))?;
                assert!(matches!(first,
                super::ChannelRenderEvent::Driver(ChatDriverEvent::Agent(envelope))
                    if matches!(envelope.payload, AgentEvent::FinalAnswer(ref text) if text == "accepted body")));
                let second = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
                    .await
                    .map_err(|_| "observation waited for live producer EOF".to_string())?
                    .ok_or_else(|| "missing original observation".to_string())?;
                match &terminal_error {
                    Some(ChatTurnError::Observation { .. }) => assert!(matches!(second,
                    Err(echo_agent::error::ReactError::ExecutionNotSettled { run_id, execution_id })
                        if run_id == "original-run" && execution_id.as_deref() == Some("original-execution"))),
                    Some(_) => assert!(matches!(second,
                    Err(echo_agent::error::ReactError::Other(message))
                        if message == "original renderer failure")),
                    None => assert!(matches!(second,
                    Err(echo_agent::error::ReactError::Other(message))
                        if message == "channel foreground owner ended without a physical terminal receipt")),
                }
                assert!(stream.next().await.is_none());
                // sender在两个投影之后才释放，证明没有以物理收尾替代原观察。
                drop(driver_tx);
            }
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn continuation_prompt_is_delivered_before_foreground_terminal() -> Result<(), String> {
        use crate::chat_driver::TurnOutcome;
        use futures::StreamExt;

        let (driver_tx, driver_rx) = tokio::sync::mpsc::channel(4);
        let (prompt_tx, prompt_rx) = tokio::sync::broadcast::channel::<String>(2);
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        let cancellation = echo_agent::agent::CancellationToken::new();
        let mut stream = super::channel_render_event_stream(
            driver_rx,
            prompt_rx,
            terminal_rx,
            super::ChannelStreamDropGuard(cancellation),
        );
        terminal_tx
            .send(Ok(TurnOutcome::Completed))
            .map_err(|_| "channel terminal receiver closed".to_string())?;
        prompt_tx
            .send("Approve continuation?".to_string())
            .map_err(|error| error.to_string())?;

        let first = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .map_err(|_| "continuation prompt timed out".to_string())?
            .ok_or_else(|| "continuation stream closed before prompt".to_string())?
            .map_err(|error| error.to_string())?;
        if !matches!(
            first,
            super::ChannelRenderEvent::Prompt(ref prompt) if prompt == "Approve continuation?"
        ) {
            return Err("foreground terminal overtook the continuation prompt".to_string());
        }
        drop(driver_tx);
        let second = stream
            .next()
            .await
            .ok_or_else(|| "missing delayed terminal receipt".to_string())?
            .map_err(|error| error.to_string())?;
        if !matches!(
            second,
            super::ChannelRenderEvent::Terminal(TurnOutcome::Completed)
        ) {
            return Err("continuation sink closure did not publish terminal".to_string());
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    mod durable_ingress {
        use super::super::{
            ChannelRenderEvent, channel_input_address, channel_input_attempt,
            project_channel_committed_events,
        };
        use crate::chat_event_log::{ChatEventLog, ChatEventRetention};
        use crate::conversation_input::{
            ConversationInputPhase, ConversationInputService, ConversationInputSource,
            stable_scoped_input_id,
        };
        use std::sync::Arc;

        struct TestDirectory(std::path::PathBuf);

        impl TestDirectory {
            fn new(label: &str) -> Result<Self, String> {
                let path = std::env::temp_dir()
                    .join(format!("eko-channel-{label}-{}", uuid::Uuid::new_v4()));
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

        fn address() -> crate::conversation_input::ConversationInputAddress {
            channel_input_address("channel-workspace", "channel-conversation")
        }

        #[tokio::test]
        async fn duplicate_transport_message_id_has_one_durable_input() -> Result<(), String> {
            let temp = TestDirectory::new("duplicate-input")?;
            let service = ConversationInputService::new(Arc::new(
                ChatEventLog::open(temp.path(), ChatEventRetention::default())
                    .map_err(|error| error.to_string())?,
            ));
            let inbound = echo_agent::channels::InboundMessage::new(
                "qq",
                "sender-1",
                "chat-1",
                echo_agent::channels::ChatType::Direct,
                "hello",
                "transport-message-1",
            );
            let input_id = stable_scoped_input_id(
                &address(),
                ConversationInputSource::Channel,
                &inbound.message_id,
            )
            .map_err(|error| error.to_string())?;
            let first = service
                .submit(
                    address(),
                    input_id.clone(),
                    inbound.text.clone(),
                    Vec::new(),
                )
                .await
                .map_err(|error| error.to_string())?;
            let duplicate = service
                .submit(address(), input_id, "hello".to_string(), Vec::new())
                .await
                .map_err(|error| error.to_string())?;
            assert!(!first.duplicate);
            assert_eq!(first.phase, ConversationInputPhase::Persisted);
            assert!(first.attempt.is_none());
            assert!(duplicate.duplicate);
            assert_eq!(
                service
                    .list(&address())
                    .await
                    .map_err(|error| error.to_string())?
                    .items
                    .len(),
                1
            );
            Ok(())
        }

        #[tokio::test]
        async fn persisted_channel_input_survives_restart_frontier() -> Result<(), String> {
            let temp = TestDirectory::new("restart-input")?;
            let root = temp.path().join("channel-ingress");
            let service = ConversationInputService::new(Arc::new(
                ChatEventLog::open(&root, ChatEventRetention::default())
                    .map_err(|error| error.to_string())?,
            ));
            let input_id = stable_scoped_input_id(
                &address(),
                ConversationInputSource::Channel,
                "transport-restart",
            )
            .map_err(|error| error.to_string())?;
            service
                .submit(
                    address(),
                    input_id.clone(),
                    "survive restart".to_string(),
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
                .list(&address())
                .await
                .map_err(|error| error.to_string())?;
            let recovered = frontier
                .items
                .first()
                .ok_or_else(|| "restart frontier lost channel input".to_string())?;
            assert_eq!(recovered.receipt.identity.input_id, input_id);
            assert_eq!(recovered.payload.text, "survive restart");
            assert_eq!(recovered.receipt.phase, ConversationInputPhase::Persisted);
            Ok(())
        }

        #[tokio::test]
        async fn drained_channel_input_settles_once_and_never_reenters_fifo() -> Result<(), String>
        {
            let temp = TestDirectory::new("drained-input")?;
            let service = ConversationInputService::new(Arc::new(
                ChatEventLog::open(temp.path(), ChatEventRetention::default())
                    .map_err(|error| error.to_string())?,
            ));
            let input_id = stable_scoped_input_id(
                &address(),
                ConversationInputSource::Channel,
                "transport-drained",
            )
            .map_err(|error| error.to_string())?;
            service
                .submit(address(), input_id, "consume once".to_string(), Vec::new())
                .await
                .map_err(|error| error.to_string())?;
            let started = service
                .dispatch_next(&address(), "channel-root-turn".to_string())
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "channel input was not dispatched".to_string())?;
            let attempt = channel_input_attempt(&started)?;
            service
                .mailbox_accepted(attempt.clone())
                .await
                .map_err(|error| error.to_string())?;
            service
                .drained(attempt.clone())
                .await
                .map_err(|error| error.to_string())?;
            let settled = service
                .settle_attempt(&attempt, &crate::chat_driver::TurnOutcome::Completed)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(settled.phase, ConversationInputPhase::TurnSettled);
            assert!(settled.drained);
            assert!(
                service
                    .list(&address())
                    .await
                    .map_err(|error| error.to_string())?
                    .items
                    .is_empty()
            );
            assert!(
                service
                    .settle_attempt(&attempt, &crate::chat_driver::TurnOutcome::Completed,)
                    .await
                    .map_err(|error| error.to_string())?
                    .duplicate
            );
            Ok(())
        }

        #[tokio::test]
        async fn canonical_pump_receipts_reach_channel_renderer_in_commit_order()
        -> Result<(), String> {
            use crate::chat_driver::ChatDriverEvent;
            use crate::conversation_input::ConversationInputFact;
            use crate::tasks::task_runtime::RuntimeEventKind;
            use crate::tasks::task_runtime::executor::ExecEvent;

            let temp = TestDirectory::new("pump-renderer")?;
            let log = Arc::new(
                ChatEventLog::open(temp.path(), ChatEventRetention::default())
                    .map_err(|error| error.to_string())?,
            );
            let service = ConversationInputService::new(Arc::clone(&log));
            let submitted = service
                .submit(
                    address(),
                    "render-completed".to_string(),
                    "render completed input".to_string(),
                    Vec::new(),
                )
                .await
                .map_err(|error| error.to_string())?;
            let started = service
                .dispatch_next(&address(), "render-completed-turn".to_string())
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "completed render input was not dispatched".to_string())?;
            let attempt = channel_input_attempt(&started)?;
            service
                .mailbox_accepted(attempt.clone())
                .await
                .map_err(|error| error.to_string())?;
            service
                .drained(attempt.clone())
                .await
                .map_err(|error| error.to_string())?;
            log.append(
                "channel-workspace",
                Some("channel-conversation"),
                &submitted.identity.input_id,
                ChatDriverEvent::Execution(Box::new(
                    ExecEvent::subagent_attempt(
                        "channel-workspace",
                        "channel-conversation",
                        "",
                        None,
                        "background-execution",
                        RuntimeEventKind::Started,
                        serde_json::json!({"task": "background review"}),
                    )
                    .with_agent("reviewer"),
                )),
            )
            .map_err(|error| error.to_string())?;
            let message = echo_agent::channels::InboundMessage::new(
                "qq",
                "sender",
                "chat",
                echo_agent::channels::ChatType::Direct,
                "render",
                "render-completed",
            );
            let (route, mut receiver) =
                super::super::input_pump::channel_input_reply_route(&message);
            project_channel_committed_events(
                log.as_ref(),
                &submitted.identity,
                &route.render_tx,
                route.lifecycle_cursor.as_ref(),
            )
            .await?;
            service
                .settle_attempt(&attempt, &crate::chat_driver::TurnOutcome::Completed)
                .await
                .map_err(|error| error.to_string())?;
            project_channel_committed_events(
                log.as_ref(),
                &submitted.identity,
                &route.render_tx,
                route.lifecycle_cursor.as_ref(),
            )
            .await?;

            let mut completed_phases = Vec::new();
            let mut saw_background_execution = false;
            while let Ok(event) = receiver.render_rx.try_recv() {
                match event {
                    ChannelRenderEvent::Driver(ChatDriverEvent::InputLifecycle(fact)) => {
                        completed_phases.push(match fact.as_ref() {
                            ConversationInputFact::Persisted { .. } => "persisted",
                            ConversationInputFact::AttemptStarted { .. } => "attempt_started",
                            ConversationInputFact::TaskRunTargetBound { .. } => continue,
                            ConversationInputFact::MailboxAccepted { .. } => "mailbox_accepted",
                            ConversationInputFact::Drained { .. }
                            | ConversationInputFact::DriverAccepted { .. } => "drained",
                            ConversationInputFact::TurnSettled { .. } => "turn_settled",
                            ConversationInputFact::Deferred { .. } => "deferred",
                            ConversationInputFact::Reordered { .. } => "reordered",
                            ConversationInputFact::RecoveryRequired { .. }
                            | ConversationInputFact::RecoveryUncertain { .. } => {
                                "recovery_required"
                            }
                            ConversationInputFact::Cancelled { .. } => "cancelled",
                        });
                    }
                    ChannelRenderEvent::Driver(ChatDriverEvent::Execution(event)) => {
                        saw_background_execution =
                            event.subagent_run_id.as_deref() == Some("background-execution");
                    }
                    _ => {}
                }
            }
            assert!(saw_background_execution);
            assert_eq!(
                completed_phases,
                vec![
                    "persisted",
                    "attempt_started",
                    "mailbox_accepted",
                    "drained",
                    "turn_settled",
                ]
            );

            let recovery = service
                .submit(
                    address(),
                    "render-cancelled".to_string(),
                    "render cancelled input".to_string(),
                    Vec::new(),
                )
                .await
                .map_err(|error| error.to_string())?;
            let recovery_started = service
                .dispatch_next(&address(), "render-cancelled-turn".to_string())
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "cancelled render input was not dispatched".to_string())?;
            let recovery_attempt = channel_input_attempt(&recovery_started)?;
            service
                .recovery_required(
                    recovery_attempt.clone(),
                    "injected renderer recovery".to_string(),
                )
                .await
                .map_err(|error| error.to_string())?;
            let recovery_message = echo_agent::channels::InboundMessage::new(
                "qq",
                "sender",
                "chat",
                echo_agent::channels::ChatType::Direct,
                "recover",
                "render-cancelled",
            );
            let (recovery_route, mut recovery_receiver) =
                super::super::input_pump::channel_input_reply_route(&recovery_message);
            project_channel_committed_events(
                log.as_ref(),
                &recovery.identity,
                &recovery_route.render_tx,
                recovery_route.lifecycle_cursor.as_ref(),
            )
            .await?;
            service
                .settle_attempt(
                    &recovery_attempt,
                    &crate::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "observer",
                            "injected observer failure",
                        ),
                    ),
                )
                .await
                .map_err(|error| error.to_string())?;
            project_channel_committed_events(
                log.as_ref(),
                &recovery.identity,
                &recovery_route.render_tx,
                recovery_route.lifecycle_cursor.as_ref(),
            )
            .await?;
            let mut saw_cancelled = false;
            while let Ok(event) = recovery_receiver.render_rx.try_recv() {
                if matches!(
                    event,
                    ChannelRenderEvent::Driver(ChatDriverEvent::InputLifecycle(fact))
                        if matches!(fact.as_ref(), ConversationInputFact::Cancelled { .. })
                ) {
                    saw_cancelled = true;
                }
            }
            assert!(saw_cancelled);
            Ok(())
        }
    }

    // ── channel attachment transport tests ──────────────────────────────
    #[cfg(feature = "channels")]
    mod multimodal {
        use super::super::{
            ChannelTurnPreparation, channel_attachment_data, prepare_channel_turn,
            stage_channel_attachments,
        };
        use echo_agent::channels::{AttachmentKind, MessageAttachment};
        use std::path::{Path, PathBuf};

        struct TestDirectory(PathBuf);

        impl TestDirectory {
            fn new(label: &str) -> Result<Self, String> {
                let path = std::env::temp_dir()
                    .join(format!("eko-channel-{label}-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
                Ok(Self(path))
            }

            fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TestDirectory {
            fn drop(&mut self) {
                let _cleanup = std::fs::remove_dir_all(&self.0);
            }
        }

        fn prepare_scoped_attachment(
            root: &Path,
            conversation_id: &str,
            turn_id: &str,
        ) -> Result<(PathBuf, PathBuf), String> {
            let attachment =
                MessageAttachment::new(AttachmentKind::File, b"workspace note".to_vec())
                    .with_filename("notes.txt");
            let staged = stage_channel_attachments(&[attachment], root)?;
            let staged_path = staged
                .first()
                .map(|attachment| attachment.path.clone())
                .ok_or_else(|| "channel attachment was not staged".to_string())?;
            let spill_dir = crate::prepared_turn::resolve_user_input_spill_dir(Some(root));
            let turn = crate::prepared_turn::PreparedUserTurn::build(
                crate::prepared_turn::UserTurnInput {
                    text: "inspect the attachment",
                    attachments: &staged,
                    spill_dir: &spill_dir,
                    conversation_id: Some(conversation_id),
                    turn_id: Some(turn_id),
                },
            )
            .map_err(|error| error.to_string())?;
            let main_refs = turn.inline_attachment_refs();
            let subagent_refs = turn.inline_attachment_refs();
            let main_path = main_refs
                .first()
                .map(|attachment| attachment.path.clone())
                .ok_or_else(|| "main Agent attachment ref is missing".to_string())?;
            let subagent_path = subagent_refs
                .first()
                .map(|attachment| attachment.path.clone())
                .ok_or_else(|| "Subagent attachment ref is missing".to_string())?;
            if main_path != subagent_path {
                return Err("main Agent and Subagent received different artifact refs".to_string());
            }
            Ok((staged_path, main_path))
        }

        #[test]
        fn image_attachment_keeps_name_and_image_mime() {
            let att = MessageAttachment::new(AttachmentKind::Image, vec![1, 2, 3])
                .with_filename("photo.png");
            let data = channel_attachment_data(0, &att);
            assert_eq!(data.name, "photo.png");
            assert_eq!(data.mime_type, "image/png");
            assert_eq!(data.size, 3);
        }

        #[test]
        fn file_attachment_keeps_inferred_text_mime() {
            let att = MessageAttachment::new(AttachmentKind::File, vec![9, 9, 9])
                .with_filename("notes.txt");
            let data = channel_attachment_data(0, &att);
            assert_eq!(data.name, "notes.txt");
            assert_eq!(data.mime_type, "text/plain");
            assert_eq!(data.size, 3);
        }

        #[tokio::test]
        async fn bounded_prepare_retires_staging_into_exact_turn_scope() -> Result<(), String> {
            let temporary = TestDirectory::new("bounded-prepare")?;
            let workspace = temporary.path().join("workspace");
            let attachment =
                MessageAttachment::new(AttachmentKind::File, b"workspace note".to_vec())
                    .with_filename("notes.txt");
            let product_data_io = crate::product_data_io::ProductDataIoService::new();
            let flow = product_data_io
                .begin_owned_flow("prepare channel user turn fixture")
                .map_err(|error| error.to_string())?;
            let turn = prepare_channel_turn(
                ChannelTurnPreparation {
                    attachments: vec![channel_attachment_data(0, &attachment)],
                    execution_root: workspace.clone(),
                    text: "inspect the attachment".to_string(),
                    conversation_id: "conversation-a".to_string(),
                    turn_id: "turn-a".to_string(),
                    runtime_authored: false,
                    workspace_io_receipt: crate::state::ScopedWorkspaceIoReceipt::global_for_test(
                        workspace.clone(),
                    ),
                },
                &flow,
            )
            .await?;
            flow.settle(None);
            let resource = turn
                .resources
                .first()
                .ok_or_else(|| "prepared attachment resource is missing".to_string())?;
            assert!(
                resource.path.starts_with(
                    workspace
                        .join(".eko/artifacts/user-input")
                        .join("conversation-a")
                        .join("turn-a")
                )
            );
            let uploads = workspace.join(".eko/uploads");
            let staging_is_empty = match std::fs::read_dir(&uploads) {
                Ok(mut entries) => entries.next().is_none(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => return Err(error.to_string()),
            };
            assert!(staging_is_empty);
            Ok(())
        }

        #[test]
        fn attachments_follow_exact_workspace_and_conversation_scope() -> Result<(), String> {
            let temporary = TestDirectory::new("attachments")?;
            let workspace_a = temporary.path().join("workspace-a");
            let workspace_b = temporary.path().join("workspace-b");
            let (staged_a1, scoped_a1) =
                prepare_scoped_attachment(&workspace_a, "conversation-a", "turn-1")?;
            let (staged_a2, scoped_a2) =
                prepare_scoped_attachment(&workspace_a, "conversation-b", "turn-1")?;
            let (staged_b1, scoped_b1) =
                prepare_scoped_attachment(&workspace_b, "conversation-a", "turn-1")?;

            assert!(staged_a1.starts_with(workspace_a.join(".eko/uploads")));
            assert!(staged_a2.starts_with(workspace_a.join(".eko/uploads")));
            assert!(staged_b1.starts_with(workspace_b.join(".eko/uploads")));
            assert!(!staged_a1.exists(), "prepared turn retires staging file");
            assert!(!staged_a2.exists(), "prepared turn retires staging file");
            assert!(!staged_b1.exists(), "prepared turn retires staging file");

            let spill_a = crate::prepared_turn::resolve_user_input_spill_dir(Some(&workspace_a));
            let spill_b = crate::prepared_turn::resolve_user_input_spill_dir(Some(&workspace_b));
            assert!(scoped_a1.starts_with(spill_a.join("conversation-a").join("turn-1")));
            assert!(scoped_a2.starts_with(spill_a.join("conversation-b").join("turn-1")));
            assert!(scoped_b1.starts_with(spill_b.join("conversation-a").join("turn-1")));
            assert_ne!(scoped_a1, scoped_a2);
            assert_ne!(scoped_a1, scoped_b1);
            Ok(())
        }

        #[test]
        fn long_text_spill_isolated_across_workspace_and_conversation() -> Result<(), String> {
            let temporary = TestDirectory::new("long-text")?;
            let workspace_a = temporary.path().join("workspace-a");
            let workspace_b = temporary.path().join("workspace-b");
            let long_text = "中文🙂tool-output\n".repeat(3_000);

            let prepare = |root: &Path, conversation_id: &str| -> Result<PathBuf, String> {
                let spill_dir = crate::prepared_turn::resolve_user_input_spill_dir(Some(root));
                let turn = crate::prepared_turn::PreparedUserTurn::build(
                    crate::prepared_turn::UserTurnInput {
                        text: &long_text,
                        attachments: &[],
                        spill_dir: &spill_dir,
                        conversation_id: Some(conversation_id),
                        turn_id: Some("turn-1"),
                    },
                )
                .map_err(|error| error.to_string())?;
                turn.resources
                    .first()
                    .map(|resource| resource.path.clone())
                    .ok_or_else(|| "long channel text did not spill".to_string())
            };

            let a1 = prepare(&workspace_a, "conversation-a")?;
            let a2 = prepare(&workspace_a, "conversation-b")?;
            let b1 = prepare(&workspace_b, "conversation-a")?;
            assert!(
                a1.starts_with(workspace_a.join(".eko/artifacts/user-input/conversation-a/turn-1"))
            );
            assert!(
                a2.starts_with(workspace_a.join(".eko/artifacts/user-input/conversation-b/turn-1"))
            );
            assert!(
                b1.starts_with(workspace_b.join(".eko/artifacts/user-input/conversation-a/turn-1"))
            );
            assert_ne!(a1, a2);
            assert_ne!(a1, b1);
            Ok(())
        }
    }

    // ── aggregate_by_sentence 测试(需 channels feature)──────────────────────
    #[cfg(feature = "channels")]
    mod aggregate {
        use super::super::{
            CHANNEL_TOOL_OUTPUT_CHARS, ChannelBufferOutcome, ChannelOutboundDraft,
            ChannelRenderEvent, ChannelStreamingSanitizer, ChannelToolAddress,
            ChannelToolObserveOutcome, ChannelToolRenderState, ChannelToolTerminal,
            FLUSH_THRESHOLD, aggregate_by_sentence, channel_outbound_transport,
            channel_outbound_transport_unpaced, channel_rate_deadline, channel_rate_policy,
            channel_tool_args_preview,
        };
        use crate::tool_execution::{
            ToolExecutionOwner, ToolExecutionStatus, ToolExecutionSummary,
        };
        use crate::tool_execution_projection::{
            ToolExecutionProjectionKind, ToolExecutionProjectionUpdate,
        };
        use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity, ToolInvocation};
        use echo_agent::channels::{ChatType, OutboundMessage};
        use echo_agent::error::Result;
        use echo_agent::tools::{
            ToolFailureCategory, ToolOutputChannel, ToolResult, ToolStreamEvent,
        };
        use futures::stream::{BoxStream, StreamExt};
        use std::path::{Path, PathBuf};

        struct TestDirectory(PathBuf);

        impl TestDirectory {
            fn new(label: &str) -> std::result::Result<Self, String> {
                let path = std::env::temp_dir().join(format!(
                    "eko-channel-render-{label}-{}",
                    uuid::Uuid::new_v4()
                ));
                std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
                Ok(Self(path))
            }

            fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TestDirectory {
            fn drop(&mut self) {
                let _cleanup = std::fs::remove_dir_all(&self.0);
            }
        }

        fn events_to_stream(
            events: Vec<Result<AgentEvent>>,
        ) -> BoxStream<'static, Result<ChannelRenderEvent>> {
            let identity = match EventIdentity::new("channel-test-stream", "channel-test") {
                Ok(identity) => identity,
                Err(error) => return futures::stream::once(async { Err(error) }).boxed(),
            };
            futures::stream::iter(events.into_iter().enumerate().map(move |(index, event)| {
                event.and_then(|payload| {
                    let sequence = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
                    EventEnvelope::new(&identity, sequence, None, payload).map(|envelope| {
                        ChannelRenderEvent::Driver(crate::chat_driver::ChatDriverEvent::Agent(
                            Box::new(envelope),
                        ))
                    })
                })
            }))
            .boxed()
        }

        fn agent_render_event(sequence: u64, payload: AgentEvent) -> Result<ChannelRenderEvent> {
            let identity = EventIdentity::new("channel-tool-stream", "channel-tool-turn")?;
            EventEnvelope::new(&identity, sequence, None, payload).map(|envelope| {
                ChannelRenderEvent::Driver(crate::chat_driver::ChatDriverEvent::Agent(Box::new(
                    envelope,
                )))
            })
        }

        fn projection_event(
            call_id: &str,
            name: &str,
            args_preview: &str,
            status: ToolExecutionStatus,
            kind: ToolExecutionProjectionKind,
        ) -> ChannelRenderEvent {
            ChannelRenderEvent::ToolProjection(ToolExecutionProjectionUpdate {
                kind,
                agent: "echo-assistant".to_string(),
                summary: ToolExecutionSummary {
                    id: format!("tool-{call_id}"),
                    call_id: call_id.to_string(),
                    owner: ToolExecutionOwner::Chat {
                        message_id: "message-1".to_string(),
                    },
                    workspace_id: "workspace-a".to_string(),
                    conversation_id: Some("conversation-a".to_string()),
                    run_id: None,
                    name: name.to_string(),
                    args_preview: args_preview.to_string(),
                    status,
                    started_at: 1,
                    finished_at: (status != ToolExecutionStatus::Running).then_some(2),
                    duration_ms: (status != ToolExecutionStatus::Running).then_some(1),
                    detail_ref: format!("chat/message-1/{call_id}"),
                },
            })
        }

        fn render_events_to_stream(
            events: Vec<Result<ChannelRenderEvent>>,
        ) -> BoxStream<'static, Result<ChannelRenderEvent>> {
            futures::stream::iter(events).boxed()
        }

        async fn collect_texts(s: BoxStream<'_, Result<OutboundMessage>>) -> Vec<String> {
            let mut out = Vec::new();
            let mut s = s;
            while let Some(item) = s.next().await {
                match item {
                    Ok(m) => out.push(m.text),
                    Err(_) => break,
                }
            }
            out
        }

        #[tokio::test]
        async fn input_target_binding_metadata_does_not_render_as_reordered() -> Result<()> {
            use crate::conversation_input::{
                ConversationInputAddress, ConversationInputAttempt, ConversationInputFact,
                ConversationInputIdentity, ConversationInputObservation,
            };
            let identity = ConversationInputIdentity {
                address: ConversationInputAddress {
                    workspace_id: "renderer-workspace".to_string(),
                    conversation_id: "renderer-conversation".to_string(),
                },
                input_id: "metadata-only".to_string(),
                revision: 1,
                payload_sha256: "a".repeat(64),
            };
            let attempt = ConversationInputAttempt {
                identity: identity.clone(),
                attempt: 1,
                attempt_id: "renderer-attempt".to_string(),
                turn_id: "renderer-turn".to_string(),
                observation: ConversationInputObservation::default(),
            };
            let reordered_identity = ConversationInputIdentity {
                input_id: "queue-control".to_string(),
                ..identity
            };
            let events = render_events_to_stream(vec![
                Ok(ChannelRenderEvent::Driver(
                    crate::chat_driver::ChatDriverEvent::InputLifecycle(Box::new(
                        ConversationInputFact::TaskRunTargetBound {
                            attempt,
                            run_id: "renderer-run".to_string(),
                            bound_at_ms: 1,
                        },
                    )),
                )),
                Ok(ChannelRenderEvent::Driver(
                    crate::chat_driver::ChatDriverEvent::InputLifecycle(Box::new(
                        ConversationInputFact::Reordered {
                            anchor: reordered_identity,
                            input_ids: vec!["queue-control".to_string()],
                            reordered_at_ms: 2,
                        },
                    )),
                )),
            ]);
            let mut output =
                aggregate_by_sentence(events, "qq".into(), "u1".into(), ChatType::Direct).await;
            let mut texts = Vec::new();
            while let Some(message) = output.next().await {
                texts.push(message?.text);
            }
            assert!(
                texts.is_empty(),
                "内部输入绑定和排队事实保留在journal，不主动发送到IM"
            );
            Ok(())
        }

        #[tokio::test]
        async fn native_blocks_keep_newlines_and_order() {
            // 原生块保留换行与顺序；不再依赖匿名token的切句边界。
            let evs = events_to_stream(vec![
                Ok(super::native_commentary_block("a", 1)),
                Ok(super::native_commentary_block("b\n", 2)),
                Ok(super::native_commentary_block("c", 3)),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts.concat(), "ab\nc");
        }

        #[tokio::test]
        async fn native_block_completion_keeps_cjk_sentence_text() {
            // complete原生块保留中文句末标点。
            let evs = events_to_stream(vec![
                Ok(super::native_commentary_block("你好。", 1)),
                Ok(super::native_commentary_block("再见", 2)),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts.concat(), "你好。再见");
        }

        #[tokio::test]
        async fn native_long_block_keeps_complete_body() {
            // 完整原生块超过旧字符阈值时仍原样投影。
            let n = FLUSH_THRESHOLD + 10;
            let long: String = "x".repeat(n);
            let evs = events_to_stream(vec![
                Ok(super::native_commentary_block(long.clone(), 1)),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts.len(), 1, "threshold flush yields 1");
            assert_eq!(texts.first().map(|text| text.chars().count()), Some(n));
        }

        #[tokio::test]
        async fn runtime_final_notification_flushes_incomplete_native_block() {
            // 真实FinalAnswer通知仍可冲刷尚未complete的原生公开块。
            let mut pending = super::native_commentary_block("hi", 1);
            if let AgentEvent::AssistantMessage { block } = &mut pending {
                block.complete = false;
            }
            let evs = events_to_stream(vec![
                Ok(pending),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts, vec!["hi".to_string()]);
        }

        #[tokio::test]
        async fn empty_buf_finalanswer_no_yield() {
            // 无原生正文的空FinalAnswer不生成消息。
            let evs = events_to_stream(vec![Ok(AgentEvent::FinalAnswer(String::new()))]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert!(
                texts.is_empty(),
                "empty runtime final without native text yields nothing"
            );
        }

        #[tokio::test]
        async fn cancelled_terminal_is_user_visible() {
            let driver = events_to_stream(vec![
                Ok(super::native_commentary_block("partial", 1)),
                Ok(AgentEvent::Cancelled),
            ]);
            let terminal = futures::stream::once(async {
                Ok(ChannelRenderEvent::Terminal(
                    crate::chat_driver::TurnOutcome::Cancelled,
                ))
            });
            let evs = driver.chain(terminal).boxed();
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(
                texts,
                vec!["partial".to_string(), "本次处理已取消。".to_string()]
            );
        }

        #[tokio::test]
        async fn failed_terminal_is_typed_and_user_visible() -> std::result::Result<(), String> {
            let driver = events_to_stream(vec![
                Ok(super::native_commentary_block("partial", 1)),
                Ok(AgentEvent::error_message("llm", "boom")),
            ]);
            let terminal = futures::stream::once(async {
                Ok(ChannelRenderEvent::Terminal(
                    crate::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message("llm_network", "boom"),
                    ),
                ))
            });
            let evs = driver.chain(terminal).boxed();
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let mut s = out;
            let first = s
                .next()
                .await
                .ok_or_else(|| "missing partial output".to_string())?
                .map_err(|error| error.to_string())?;
            assert_eq!(first.text, "partial");
            let second = s
                .next()
                .await
                .ok_or_else(|| "missing typed terminal output".to_string())?
                .map_err(|error| error.to_string())?;
            assert_eq!(second.text, "本次处理失败：boom");
            Ok(())
        }

        #[tokio::test]
        async fn tool_lifecycle_is_ordered_redacted_and_bounded() -> std::result::Result<(), String>
        {
            let secret = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
            let args = serde_json::json!({
                "path": "src/main.rs",
                "token": secret,
                "query": "中文🙂".repeat(120)
            });
            let invocation = ToolInvocation {
                requested_name: "grep".to_string(),
                requested_args: args.clone(),
                name: "grep".to_string(),
                args: args.clone(),
                rewrites: Vec::new(),
            };
            let args_preview = channel_tool_args_preview(&args);
            let success = ToolResult::success("成功🙂".repeat(300))
                .with_truncated(true)
                .with_artifact(echo_agent::tools::artifact::ToolOutputArtifactRef {
                    path: "/tmp/channel-tool-full.log".into(),
                    artifact_bytes: 8_192,
                    payload_bytes: 8_192,
                    sha256: "unverified-artifact".to_string(),
                    retention: "temporary_1h".to_string(),
                });
            let complete_failure = ToolResult::failure(
                ToolFailureCategory::Timeout,
                "token=secretvalue123456，请重试🙂",
            );
            let terminal_failure = ToolResult::failure(
                ToolFailureCategory::InvalidArguments,
                "password=secretvalue123456，参数错误🙂",
            );

            let events = render_events_to_stream(vec![
                Ok(projection_event(
                    "call-1",
                    "grep",
                    &args_preview,
                    ToolExecutionStatus::Running,
                    ToolExecutionProjectionKind::Started,
                )),
                agent_render_event(
                    1,
                    AgentEvent::ToolCall {
                        call_id: "call-1".to_string(),
                        invocation,
                    },
                ),
                agent_render_event(
                    2,
                    AgentEvent::ToolStream {
                        call_id: "call-1".to_string(),
                        name: "grep".to_string(),
                        event: ToolStreamEvent::Progress {
                            message: format!("正在搜索🙂 Bearer {secret}"),
                            percent: Some(25),
                        },
                    },
                ),
                agent_render_event(
                    3,
                    AgentEvent::ToolStream {
                        call_id: "call-1".to_string(),
                        name: "grep".to_string(),
                        event: ToolStreamEvent::Output {
                            channel: ToolOutputChannel::Stdout,
                            chunk: "stdout🙂".to_string(),
                        },
                    },
                ),
                agent_render_event(
                    4,
                    AgentEvent::ToolStream {
                        call_id: "call-1".to_string(),
                        name: "grep".to_string(),
                        event: ToolStreamEvent::Output {
                            channel: ToolOutputChannel::Stderr,
                            chunk: "stderr🙂".to_string(),
                        },
                    },
                ),
                agent_render_event(
                    5,
                    AgentEvent::ToolStream {
                        call_id: "call-1".to_string(),
                        name: "grep".to_string(),
                        event: ToolStreamEvent::Output {
                            channel: ToolOutputChannel::Log,
                            chunk: "日志🙂".to_string(),
                        },
                    },
                ),
                agent_render_event(
                    6,
                    AgentEvent::ToolStream {
                        call_id: "call-1".to_string(),
                        name: "grep".to_string(),
                        event: ToolStreamEvent::Output {
                            channel: ToolOutputChannel::Stdout,
                            chunk: "长输出🙂".repeat(CHANNEL_TOOL_OUTPUT_CHARS),
                        },
                    },
                ),
                Ok(projection_event(
                    "call-1",
                    "grep",
                    &args_preview,
                    ToolExecutionStatus::Succeeded,
                    ToolExecutionProjectionKind::Finished,
                )),
                agent_render_event(
                    7,
                    AgentEvent::ToolResult {
                        call_id: "call-1".to_string(),
                        name: "grep".to_string(),
                        result: success,
                    },
                ),
                Ok(projection_event(
                    "call-2",
                    "shell",
                    "{\"command\":\"false\"}",
                    ToolExecutionStatus::Running,
                    ToolExecutionProjectionKind::Started,
                )),
                agent_render_event(
                    8,
                    AgentEvent::ToolCall {
                        call_id: "call-2".to_string(),
                        invocation: ToolInvocation {
                            requested_name: "shell".to_string(),
                            requested_args: serde_json::json!({"command": "false"}),
                            name: "shell".to_string(),
                            args: serde_json::json!({"command": "false"}),
                            rewrites: Vec::new(),
                        },
                    },
                ),
                agent_render_event(
                    9,
                    AgentEvent::ToolStream {
                        call_id: "call-2".to_string(),
                        name: "shell".to_string(),
                        event: ToolStreamEvent::Complete(complete_failure),
                    },
                ),
                Ok(projection_event(
                    "call-2",
                    "shell",
                    "{\"command\":\"false\"}",
                    ToolExecutionStatus::TimedOut,
                    ToolExecutionProjectionKind::Finished,
                )),
                agent_render_event(
                    10,
                    AgentEvent::ToolResult {
                        call_id: "call-2".to_string(),
                        name: "shell".to_string(),
                        result: ToolResult::failure(
                            ToolFailureCategory::Timeout,
                            "token=secretvalue123456，请重试🙂",
                        ),
                    },
                ),
                Ok(projection_event(
                    "call-3",
                    "read_file",
                    "{\"path\":\"missing\"}",
                    ToolExecutionStatus::Running,
                    ToolExecutionProjectionKind::Started,
                )),
                agent_render_event(
                    10,
                    AgentEvent::ToolCall {
                        call_id: "call-3".to_string(),
                        invocation: ToolInvocation {
                            requested_name: "read_file".to_string(),
                            requested_args: serde_json::json!({"path": "missing"}),
                            name: "read_file".to_string(),
                            args: serde_json::json!({"path": "missing"}),
                            rewrites: Vec::new(),
                        },
                    },
                ),
                Ok(projection_event(
                    "call-3",
                    "read_file",
                    "{\"path\":\"missing\"}",
                    ToolExecutionStatus::Failed,
                    ToolExecutionProjectionKind::Finished,
                )),
                agent_render_event(
                    11,
                    AgentEvent::ToolResult {
                        call_id: "call-3".to_string(),
                        name: "read_file".to_string(),
                        result: terminal_failure,
                    },
                ),
                agent_render_event(12, AgentEvent::Cancelled),
                Ok(ChannelRenderEvent::Terminal(
                    crate::chat_driver::TurnOutcome::Cancelled,
                )),
            ]);
            let output =
                aggregate_by_sentence(events, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(output).await;
            let joined = texts.join("\n");

            assert!(!joined.contains(secret));
            assert!(!joined.contains("secretvalue123456"));
            assert!(joined.contains("[REDACTED]"));
            assert!(joined.contains("started grep"));
            assert!(joined.contains("progress grep 25%"));
            assert!(joined.contains("stdout grep"));
            assert!(joined.contains("stderr grep"));
            assert!(joined.contains("log grep"));
            assert!(joined.contains("output available in detail chat/message-1/call-1"));
            assert!(joined.contains("result grep"));
            assert!(joined.contains("truncated"));
            assert!(!joined.contains("artifact /tmp/channel-tool-full.log"));
            assert!(joined.contains("detail chat/message-1/call-1"));
            assert!(joined.contains("error shell [timeout -> verify_then_retry]"));
            assert_eq!(
                joined
                    .matches("error shell [timeout -> verify_then_retry]")
                    .count(),
                1
            );
            assert!(joined.contains("error read_file [invalid_arguments -> correct_arguments]"));
            assert!(texts.iter().all(|text| {
                text.chars().count() <= CHANNEL_TOOL_OUTPUT_CHARS.saturating_add(400)
            }));

            let started = texts
                .iter()
                .position(|text| text.contains("started grep"))
                .ok_or_else(|| "started event is missing".to_string())?;
            let progress = texts
                .iter()
                .position(|text| text.contains("progress grep"))
                .ok_or_else(|| "progress event is missing".to_string())?;
            let result = texts
                .iter()
                .position(|text| text.contains("result grep"))
                .ok_or_else(|| "result event is missing".to_string())?;
            let terminal = texts
                .iter()
                .position(|text| text == "本次处理已取消。")
                .ok_or_else(|| "terminal event is missing".to_string())?;
            assert!(started < progress);
            assert!(progress < result);
            assert!(result < terminal);
            assert_eq!(terminal, texts.len().saturating_sub(1));
            Ok(())
        }

        #[tokio::test]
        async fn only_registered_verified_artifact_is_rendered() -> std::result::Result<(), String>
        {
            let temporary = TestDirectory::new("verified-artifact")?;
            let artifact_config = echo_agent::tools::artifact::ToolOutputArtifactConfig::new(
                temporary.path().join("artifacts"),
                "conversation_or_30d",
            )
            .threshold_bytes(1);
            let mut writer = echo_agent::tools::artifact::ToolOutputArtifactWriter::new(
                artifact_config.clone(),
                echo_agent::tools::artifact::ToolOutputArtifactIdentity {
                    conversation_id: Some("conversation-a".to_string()),
                    run_id: Some("turn-a".to_string()),
                    call_id: "call-artifact".to_string(),
                    tool_name: "shell".to_string(),
                },
            );
            writer
                .push_raw("complete artifact output")
                .map_err(|error| error.to_string())?;
            let artifact = writer
                .finish()
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "artifact writer did not spill".to_string())?;
            let result = ToolResult::success("bounded preview").with_artifact(artifact.clone());
            let repository = std::sync::Arc::new(
                crate::tool_execution::ToolExecutionRepository::open(
                    temporary.path().join("tool-details"),
                )
                .map_err(|error| error.to_string())?,
            );
            repository.register_artifact_config(artifact_config);
            let owner = ToolExecutionOwner::Chat {
                message_id: "message-1".to_string(),
            };
            let invocation = ToolInvocation {
                requested_name: "shell".to_string(),
                requested_args: serde_json::json!({"command": "echo ok"}),
                name: "shell".to_string(),
                args: serde_json::json!({"command": "echo ok"}),
                rewrites: Vec::new(),
            };
            let started = repository
                .project_start(
                    "workspace-a",
                    owner,
                    Some("conversation-a"),
                    None,
                    "call-artifact",
                    &invocation,
                )
                .map_err(|error| error.to_string())?
                .summary;
            let finished = repository
                .project_finish("workspace-a", &started.owner, "call-artifact", &result)
                .map_err(|error| error.to_string())?
                .summary;
            let verified = repository
                .verified_artifact_reference("workspace-a", &finished.detail_ref)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "registered artifact was not recovered from detail".to_string())?;
            assert_eq!(verified, artifact);
            let events = render_events_to_stream(vec![
                Ok(ChannelRenderEvent::ToolProjection(
                    ToolExecutionProjectionUpdate {
                        kind: ToolExecutionProjectionKind::Started,
                        agent: "echo-assistant".to_string(),
                        summary: started,
                    },
                )),
                agent_render_event(
                    1,
                    AgentEvent::ToolCall {
                        call_id: "call-artifact".to_string(),
                        invocation,
                    },
                ),
                Ok(ChannelRenderEvent::ToolProjection(
                    ToolExecutionProjectionUpdate {
                        kind: ToolExecutionProjectionKind::Finished,
                        agent: "echo-assistant".to_string(),
                        summary: finished,
                    },
                )),
                agent_render_event(
                    2,
                    AgentEvent::ToolResult {
                        call_id: "call-artifact".to_string(),
                        name: "shell".to_string(),
                        result,
                    },
                ),
                Ok(ChannelRenderEvent::Terminal(
                    crate::chat_driver::TurnOutcome::Completed,
                )),
            ]);
            let output = super::super::aggregate_by_sentence_with_repository(
                events,
                "qq".to_string(),
                "u1".to_string(),
                ChatType::Direct,
                repository,
                None,
            )
            .await;
            let joined = collect_texts(output).await.join("\n");
            assert!(
                joined.contains(&artifact.path.to_string_lossy().to_string()),
                "verified artifact was omitted: {joined}"
            );
            assert!(joined.contains(&artifact.sha256));
            assert!(joined.contains("retention conversation_or_30d"));
            Ok(())
        }

        #[tokio::test]
        async fn main_and_subagent_same_call_id_render_independently()
        -> std::result::Result<(), String> {
            use crate::tasks::task_runtime::RuntimeEventKind;
            use crate::tasks::task_runtime::executor::ExecEvent;

            let invocation = ToolInvocation {
                requested_name: "read_file".to_string(),
                requested_args: serde_json::json!({"path": "shared.txt"}),
                name: "read_file".to_string(),
                args: serde_json::json!({"path": "shared.txt"}),
                rewrites: Vec::new(),
            };
            let subagent_summary = |status| ToolExecutionSummary {
                id: "tool-subagent-shared".to_string(),
                call_id: "shared-call".to_string(),
                owner: ToolExecutionOwner::Subagent {
                    subagent_run_id: "subagent-1".to_string(),
                },
                workspace_id: "workspace-a".to_string(),
                conversation_id: Some("conversation-a".to_string()),
                run_id: Some("run-1".to_string()),
                name: "read_file".to_string(),
                args_preview: "{\"path\":\"shared.txt\"}".to_string(),
                status,
                started_at: 1,
                finished_at: (status != ToolExecutionStatus::Running).then_some(2),
                duration_ms: (status != ToolExecutionStatus::Running).then_some(1),
                detail_ref: "subagent/subagent-1/shared-call".to_string(),
            };
            let events = render_events_to_stream(vec![
                Ok(projection_event(
                    "shared-call",
                    "read_file",
                    "{\"path\":\"shared.txt\"}",
                    ToolExecutionStatus::Running,
                    ToolExecutionProjectionKind::Started,
                )),
                Ok(ChannelRenderEvent::ToolProjection(
                    ToolExecutionProjectionUpdate {
                        kind: ToolExecutionProjectionKind::Started,
                        agent: "reviewer".to_string(),
                        summary: subagent_summary(ToolExecutionStatus::Running),
                    },
                )),
                agent_render_event(
                    1,
                    AgentEvent::ToolCall {
                        call_id: "shared-call".to_string(),
                        invocation: invocation.clone(),
                    },
                ),
                Ok(ChannelRenderEvent::Driver(
                    crate::chat_driver::ChatDriverEvent::Execution(Box::new(
                        ExecEvent::subagent(
                            "workspace-a",
                            "conversation-a",
                            "run-1",
                            "task-1",
                            "subagent-1",
                            RuntimeEventKind::ToolStarted,
                            serde_json::json!({
                                "call_id": "shared-call",
                                "invocation": invocation,
                            }),
                        )
                        .with_agent("reviewer"),
                    )),
                )),
                agent_render_event(
                    2,
                    AgentEvent::ToolStream {
                        call_id: "shared-call".to_string(),
                        name: "read_file".to_string(),
                        event: ToolStreamEvent::Output {
                            channel: ToolOutputChannel::Stdout,
                            chunk: "{\"password\":\"main-supersecret".to_string(),
                        },
                    },
                ),
                Ok(ChannelRenderEvent::Driver(
                    crate::chat_driver::ChatDriverEvent::Execution(Box::new(
                        ExecEvent::subagent(
                            "workspace-a",
                            "conversation-a",
                            "run-1",
                            "task-1",
                            "subagent-1",
                            RuntimeEventKind::ToolOutput,
                            serde_json::json!({
                                "call_id": "shared-call",
                                "name": "read_file",
                                "channel": "stdout",
                                "chunk": "{\"password\":\"subagent-supersecret",
                            }),
                        )
                        .with_agent("reviewer"),
                    )),
                )),
                Ok(projection_event(
                    "shared-call",
                    "read_file",
                    "{\"path\":\"shared.txt\"}",
                    ToolExecutionStatus::Succeeded,
                    ToolExecutionProjectionKind::Finished,
                )),
                agent_render_event(
                    3,
                    AgentEvent::ToolResult {
                        call_id: "shared-call".to_string(),
                        name: "read_file".to_string(),
                        result: ToolResult::success("main-result"),
                    },
                ),
                Ok(ChannelRenderEvent::ToolProjection(
                    ToolExecutionProjectionUpdate {
                        kind: ToolExecutionProjectionKind::Finished,
                        agent: "reviewer".to_string(),
                        summary: subagent_summary(ToolExecutionStatus::Succeeded),
                    },
                )),
                Ok(ChannelRenderEvent::Driver(
                    crate::chat_driver::ChatDriverEvent::Execution(Box::new(
                        ExecEvent::subagent(
                            "workspace-a",
                            "conversation-a",
                            "run-1",
                            "task-1",
                            "subagent-1",
                            RuntimeEventKind::ToolCompleted,
                            serde_json::json!({
                                "call_id": "shared-call",
                                "name": "read_file",
                                "result": ToolResult::success("subagent-result"),
                            }),
                        )
                        .with_agent("reviewer"),
                    )),
                )),
                Ok(ChannelRenderEvent::Terminal(
                    crate::chat_driver::TurnOutcome::Completed,
                )),
            ]);
            let joined = collect_texts(
                aggregate_by_sentence(events, "qq".to_string(), "u1".to_string(), ChatType::Direct)
                    .await,
            )
            .await
            .join("\n");
            assert!(joined.contains("[tool:shared-call] started read_file"));
            assert!(joined.contains("[subagent:subagent-1 tool:shared-call] started read_file"));
            assert!(!joined.contains("main-supersecret"));
            assert!(!joined.contains("subagent-supersecret"));
            assert!(joined.contains("output available in detail"));
            assert!(joined.contains("main-result"));
            assert!(joined.contains("subagent-result"));
            Ok(())
        }

        #[tokio::test]
        async fn every_outbound_surface_is_redacted_chunked_and_terminal_reserved()
        -> std::result::Result<(), String> {
            use crate::tasks::task_runtime::RuntimeEventKind;
            use crate::tasks::task_runtime::executor::ExecEvent;

            let secret = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
            let sensitive = format!("Bearer {secret}");
            let mut events = vec![
                Ok(ChannelRenderEvent::Prompt(format!("prompt {sensitive}"))),
                Ok(ChannelRenderEvent::Driver(
                    super::native_commentary_event(
                        4,
                        format!("huge native block {sensitive} {}", "中文🙂".repeat(5_000)),
                    )
                    .map_err(|error| error.to_string())?,
                )),
                agent_render_event(
                    1,
                    AgentEvent::BudgetDecision {
                        decision: serde_json::from_value(serde_json::json!("wind_down"))
                            .map_err(|error| error.to_string())?,
                        reason: format!("budget {sensitive}"),
                        iteration: 1,
                        reported_model_tokens: 1,
                        usage_complete: true,
                    },
                ),
                agent_render_event(
                    2,
                    AgentEvent::GuardTriggered {
                        guard: format!("guard {sensitive}"),
                        blocked: true,
                    },
                ),
                agent_render_event(
                    3,
                    AgentEvent::SafetyNotice {
                        action: format!("action {sensitive}"),
                        reason: format!("reason {sensitive}"),
                        risk: format!("risk {sensitive}"),
                        permission: format!("permission {sensitive}"),
                    },
                ),
                Ok(ChannelRenderEvent::Driver(
                    crate::chat_driver::ChatDriverEvent::Execution(Box::new(ExecEvent::run(
                        "workspace-a",
                        "conversation-a",
                        "run-a",
                        RuntimeEventKind::RunFailed,
                        serde_json::json!({"token": secret}),
                    ))),
                )),
                Ok(ChannelRenderEvent::Driver(
                    crate::chat_driver::ChatDriverEvent::Interrupt {
                        run_id: format!("run-{sensitive}"),
                        goal: format!("goal {sensitive}"),
                        new_message: format!("message {sensitive}"),
                    },
                )),
                Ok(ChannelRenderEvent::Driver(
                    crate::chat_driver::ChatDriverEvent::ApprovalRequest {
                        request_id: format!("approval-{sensitive}"),
                        identity: None,
                        tool_name: format!("tool-{sensitive}"),
                        args: serde_json::json!({"token": secret}),
                        prompt: format!("approve {sensitive}"),
                    },
                )),
                Ok(ChannelRenderEvent::Driver(
                    crate::chat_driver::ChatDriverEvent::InputRequest {
                        request_id: format!("input-{sensitive}"),
                        identity: None,
                        prompt: format!("input {sensitive}"),
                    },
                )),
                Ok(ChannelRenderEvent::Driver(
                    crate::chat_driver::ChatDriverEvent::SelectionRequest {
                        request_id: format!("selection-{sensitive}"),
                        identity: None,
                        prompt: format!("select {sensitive}"),
                        options: vec![format!("option {sensitive}")],
                        task_id: None,
                        context: None,
                        phase: None,
                    },
                )),
            ];
            for index in 0..5 {
                events.push(Ok(ChannelRenderEvent::Prompt(format!(
                    "rate-{index} {sensitive}"
                ))));
            }
            events.push(Ok(ChannelRenderEvent::Terminal(
                crate::chat_driver::TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "terminal_failure",
                    format!("terminal {sensitive}"),
                )),
            )));

            let output = aggregate_by_sentence(
                render_events_to_stream(events),
                "qq".into(),
                "u1".into(),
                ChatType::Direct,
            )
            .await;
            let texts = collect_texts(output).await;
            let joined = texts.join("\n");
            assert!(!joined.contains(secret));
            assert!(joined.contains("[REDACTED]"));
            assert!(texts.iter().all(|text| text.len() <= 1_800));
            assert!(texts.len() <= super::super::CHANNEL_OUTBOUND_TOTAL_MESSAGES);
            let terminal = texts
                .last()
                .ok_or_else(|| "terminal output is missing".to_string())?;
            assert!(terminal.starts_with("本次处理失败："));
            assert!(terminal.contains("[REDACTED]"));
            Ok(())
        }

        #[tokio::test]
        async fn outbound_total_budget_preserves_reserved_terminal_without_real_wait()
        -> std::result::Result<(), String> {
            let mut drafts = (0..300)
                .map(|index| Ok(ChannelOutboundDraft::ordinary(format!("message-{index}"))))
                .collect::<Vec<_>>();
            drafts.push(Ok(ChannelOutboundDraft::terminal("terminal")));
            let texts = collect_texts(channel_outbound_transport_unpaced(
                futures::stream::iter(drafts).boxed(),
                "qq".to_string(),
                "u1".to_string(),
                ChatType::Direct,
            ))
            .await;
            assert!(texts.len() <= super::super::CHANNEL_OUTBOUND_TOTAL_MESSAGES);
            assert_eq!(texts.last().map(String::as_str), Some("terminal"));
            assert!(texts.iter().any(|text| text.contains("additional output")));
            Ok(())
        }

        #[tokio::test]
        async fn streaming_redaction_covers_cross_draft_boundaries()
        -> std::result::Result<(), String> {
            let drafts = futures::stream::iter(vec![
                Ok(ChannelOutboundDraft::stream("gh")),
                Ok(ChannelOutboundDraft::stream(
                    "p_abcdefghijklmnopqrstuvwxyz1234567890 ",
                )),
                Ok(ChannelOutboundDraft::stream("Bearer cross")),
                Ok(ChannelOutboundDraft::stream("boundarytoken ")),
                Ok(ChannelOutboundDraft::stream("password=secret")),
                Ok(ChannelOutboundDraft::stream("value123456 ")),
                Ok(ChannelOutboundDraft::terminal("\nterminal done")),
            ])
            .boxed();
            let texts = collect_texts(channel_outbound_transport(
                drafts,
                "qq".to_string(),
                "u1".to_string(),
                ChatType::Direct,
                None,
            ))
            .await;
            let joined = texts.join("\n");
            assert!(!joined.contains("ghp_abcdefghijklmnopqrstuvwxyz1234567890"));
            assert!(!joined.contains("crossboundarytoken"));
            assert!(!joined.contains("secretvalue123456"));
            assert!(joined.matches("[REDACTED]").count() >= 3);
            assert!(
                texts
                    .last()
                    .is_some_and(|text| text.contains("terminal done"))
            );
            Ok(())
        }

        #[test]
        fn streaming_redaction_covers_every_canonical_pattern_at_every_split()
        -> std::result::Result<(), String> {
            let secrets = [
                "AKIA1234567890ABCDEF",
                "ghp_abcdefghijklmnopqrstuvwxyz1234567890",
                "github_pat_abcdefghijklmnopqrstuv",
                "sk-ant-abcdefghijklmnopqrst",
                "sk-abcdefghijklmnopqrst",
                "eyJabcdefghij.abcdefghijk.abcdefghijkl",
                "xoxb-abcdefghij",
                "hf_abcdefghijklmnopqrstuvwxyz12345678",
                "AIza1234567890abcdefghijklmnopqrstuvwxy",
                "glpat-abcdefghijklmnopqrstuvwxyz",
                "Bearer abcdefghijklmnopqrstuvwxyz",
                "password=secretvalue123456",
                "{\"password\"\n: \"secretvalue123456\"}",
                "postgresql://user:secret_password@localhost/db",
                "-----BEGIN OPENSSH PRIVATE KEY-----",
            ];
            for secret in secrets {
                let character_count = secret.chars().count();
                for split in 1..character_count {
                    let first = secret.chars().take(split).collect::<String>();
                    let second = secret.chars().skip(split).collect::<String>();
                    let mut sanitizer = ChannelStreamingSanitizer::default();
                    assert_eq!(sanitizer.push(&first), ChannelBufferOutcome::Buffered);
                    assert_eq!(
                        sanitizer.push(&format!("{second} ")),
                        ChannelBufferOutcome::Buffered
                    );
                    let rendered = sanitizer.finish().unwrap_or_default();
                    if rendered.contains(secret) || !rendered.contains("[REDACTED") {
                        return Err(format!(
                            "secret pattern escaped at split {split}: {secret} -> {rendered}"
                        ));
                    }
                }
            }
            Ok(())
        }

        #[test]
        fn streaming_redaction_holds_long_unclosed_json_secret_until_escaped_close()
        -> std::result::Result<(), String> {
            let secret_fragment = "秘密🙂A".repeat(700);
            let drafts = [
                "{\"password\"\n: \"".to_string(),
                secret_fragment.clone(),
                "escaped\\\"quote".to_string(),
                "\",\"visible\":true}".to_string(),
            ];
            let mut sanitizer = ChannelStreamingSanitizer::default();
            for draft in drafts {
                assert_eq!(sanitizer.push(&draft), ChannelBufferOutcome::Buffered);
            }
            let rendered = sanitizer.finish().unwrap_or_default();
            assert!(!rendered.contains("秘密"));
            assert!(!rendered.contains("escaped"));
            assert!(!rendered.contains("quote"));
            assert_eq!(rendered.matches("[REDACTED]").count(), 1);
            assert!(rendered.contains("\"visible\":true"));
            Ok(())
        }

        #[test]
        fn buffered_redaction_covers_long_db_jwt_and_nested_json_candidates()
        -> std::result::Result<(), String> {
            let db_password = "A".repeat(5_000);
            let jwt_segment = "B".repeat(5_000);
            let nested_secret = "秘密🙂".repeat(1_500);
            let candidates = [
                (
                    format!("postgresql://user:{db_password}"),
                    "@localhost/db ".to_string(),
                    db_password,
                ),
                (
                    format!("eyJ{jwt_segment}"),
                    ".abcdefghijk.abcdefghijkl ".to_string(),
                    jwt_segment,
                ),
                (
                    "{\"password\":{\"nested\":[\"".to_string(),
                    format!("{nested_secret}\",\"other\"]}},\"after\":\"ok\"}}"),
                    nested_secret,
                ),
            ];
            for (first, second, secret) in candidates {
                let mut sanitizer = ChannelStreamingSanitizer::default();
                assert_eq!(sanitizer.push(&first), ChannelBufferOutcome::Buffered);
                assert_eq!(sanitizer.push(&second), ChannelBufferOutcome::Buffered);
                let rendered = sanitizer.finish().unwrap_or_default();
                assert!(!rendered.contains(&secret));
                assert!(rendered.contains("[REDACTED]"));
            }
            Ok(())
        }

        #[tokio::test]
        async fn buffered_safe_content_preserves_100k_before_transport_cap()
        -> std::result::Result<(), String> {
            let safe = "safe-中文🙂\n".repeat(7_000);
            assert!(safe.len() > 100_000);
            let mut buffer = ChannelStreamingSanitizer::default();
            assert_eq!(buffer.push(&safe), ChannelBufferOutcome::Buffered);
            assert_eq!(buffer.finish().as_deref(), Some(safe.as_str()));
            let mut drafts = Vec::new();
            let mut chunk = String::new();
            for character in safe.chars() {
                if !chunk.is_empty() && chunk.len().saturating_add(character.len_utf8()) > 4_000 {
                    drafts.push(Ok(ChannelOutboundDraft::stream(std::mem::take(&mut chunk))));
                }
                chunk.push(character);
            }
            if !chunk.is_empty() {
                drafts.push(Ok(ChannelOutboundDraft::stream(chunk)));
            }
            drafts.push(Ok(ChannelOutboundDraft::terminal("terminal")));
            let texts = collect_texts(channel_outbound_transport_unpaced(
                futures::stream::iter(drafts).boxed(),
                "qq".to_string(),
                "u1".to_string(),
                ChatType::Direct,
            ))
            .await;
            let terminal = texts.last().cloned().unwrap_or_default();
            let recovered = texts
                .iter()
                .take(texts.len().saturating_sub(1))
                .cloned()
                .collect::<String>();
            assert_eq!(recovered, safe);
            assert_eq!(terminal, "terminal");
            Ok(())
        }

        #[tokio::test]
        async fn buffered_overflow_and_literal_marker_are_typed_not_inferred()
        -> std::result::Result<(), String> {
            let drafts = futures::stream::iter(vec![
                Ok(ChannelOutboundDraft::stream("literal [REDACTED]foo")),
                Ok(ChannelOutboundDraft::stream("visible123 ")),
                Ok(ChannelOutboundDraft::terminal("terminal")),
            ])
            .boxed();
            let literal = collect_texts(channel_outbound_transport_unpaced(
                drafts,
                "qq".to_string(),
                "u1".to_string(),
                ChatType::Direct,
            ))
            .await
            .join("");
            assert!(literal.contains("literal [REDACTED]foovisible123 "));

            let overflow = futures::stream::iter(vec![
                Ok(ChannelOutboundDraft::stream("x".repeat(260 * 1024))),
                Ok(ChannelOutboundDraft::terminal("terminal")),
            ])
            .boxed();
            let texts = collect_texts(channel_outbound_transport_unpaced(
                overflow,
                "qq".to_string(),
                "u1".to_string(),
                ChatType::Direct,
            ))
            .await;
            assert!(texts.iter().any(|text| text.contains("retention limit")));
            assert_eq!(texts.last().map(String::as_str), Some("terminal"));
            assert!(!texts.join("").contains(&"x".repeat(128)));
            Ok(())
        }

        #[test]
        fn outbound_rate_fake_clock_allows_burst_then_paces_sustained_messages() {
            let policy = channel_rate_policy("qq");
            let started = tokio::time::Instant::now();
            let mut remaining_burst = policy.burst;
            let mut next_sustained = started + policy.sustained_interval;
            for _ in 0..4 {
                assert_eq!(
                    channel_rate_deadline(
                        &mut remaining_burst,
                        &mut next_sustained,
                        started,
                        policy,
                    ),
                    None
                );
            }
            assert_eq!(
                channel_rate_deadline(&mut remaining_burst, &mut next_sustained, started, policy,),
                Some(started + std::time::Duration::from_millis(250))
            );
            assert_eq!(
                channel_rate_deadline(
                    &mut remaining_burst,
                    &mut next_sustained,
                    started + std::time::Duration::from_millis(250),
                    policy,
                ),
                Some(started + std::time::Duration::from_millis(500))
            );
        }

        #[test]
        fn channel_preserves_canonical_cli_tui_tool_fields() -> std::result::Result<(), String> {
            let args = serde_json::json!({"path": "src/lib.rs", "offset": 12});
            let preview = channel_tool_args_preview(&args);
            let mut state = ChannelToolRenderState::default();
            let update = match projection_event(
                "call-shared",
                "read_file",
                &preview,
                ToolExecutionStatus::Running,
                ToolExecutionProjectionKind::Started,
            ) {
                ChannelRenderEvent::ToolProjection(update) => update,
                _ => return Err("fixture did not produce a tool projection".to_string()),
            };
            assert_eq!(state.observe(update), ChannelToolObserveOutcome::Accepted);
            let entry = state
                .entries
                .get("tool-call-shared")
                .ok_or_else(|| "channel tool state did not preserve call id".to_string())?;

            // CLI and TUI consume these exact canonical AgentEvent/product
            // fields too; only their presentation differs from channel text.
            assert_eq!(entry.summary.call_id, "call-shared");
            assert_eq!(entry.summary.name, "read_file");
            assert_eq!(entry.summary.args_preview, preview);
            assert_eq!(entry.summary.status, ToolExecutionStatus::Running);
            assert_eq!(entry.summary.detail_ref, "chat/message-1/call-shared");
            Ok(())
        }

        #[test]
        fn active_tools_are_bounded_and_owner_qualified() -> std::result::Result<(), String> {
            let mut literal_state = ChannelToolRenderState::default();
            let literal = match projection_event(
                "literal-marker",
                "shell",
                "{}",
                ToolExecutionStatus::Running,
                ToolExecutionProjectionKind::Started,
            ) {
                ChannelRenderEvent::ToolProjection(update) => update,
                _ => return Err("fixture did not produce literal marker projection".to_string()),
            };
            let literal_address = ChannelToolAddress::from_summary(&literal.summary);
            assert_eq!(
                literal_state.observe(literal),
                ChannelToolObserveOutcome::Accepted
            );
            let fixed = literal_state
                .output_preview(&literal_address, "ok...[TRUNCATED]")
                .ok_or_else(|| "fixed output notice is missing".to_string())?;
            assert!(!fixed.contains("ok...[TRUNCATED]"));
            assert!(fixed.contains("output available in detail"));
            let later = literal_state
                .output_preview(&literal_address, "later")
                .ok_or_else(|| "second fixed output notice is missing".to_string())?;
            assert!(!later.contains("later"));

            let mut state = ChannelToolRenderState::default();
            for index in 0..super::super::CHANNEL_ACTIVE_TOOL_LIMIT {
                let update = match projection_event(
                    &format!("call-{index}"),
                    "read_file",
                    "{}",
                    ToolExecutionStatus::Running,
                    ToolExecutionProjectionKind::Started,
                ) {
                    ChannelRenderEvent::ToolProjection(update) => update,
                    _ => return Err("fixture did not produce a tool projection".to_string()),
                };
                assert_eq!(state.observe(update), ChannelToolObserveOutcome::Accepted);
            }
            let overflow = match projection_event(
                "overflow",
                "read_file",
                "{}",
                ToolExecutionStatus::Running,
                ToolExecutionProjectionKind::Started,
            ) {
                ChannelRenderEvent::ToolProjection(update) => update,
                _ => return Err("fixture did not produce overflow projection".to_string()),
            };
            assert_eq!(state.observe(overflow), ChannelToolObserveOutcome::Capacity);
            assert_eq!(state.entries.len(), super::super::CHANNEL_ACTIVE_TOOL_LIMIT);

            let main = ChannelToolAddress::chat(
                "workspace-a",
                Some("conversation-a"),
                None,
                "message-1",
                "call-0",
            );
            let subagent_summary = ToolExecutionSummary {
                id: "tool-subagent-collision".to_string(),
                call_id: "call-0".to_string(),
                owner: ToolExecutionOwner::Subagent {
                    subagent_run_id: "subagent-1".to_string(),
                },
                workspace_id: "workspace-a".to_string(),
                conversation_id: Some("conversation-a".to_string()),
                run_id: Some("run-1".to_string()),
                name: "read_file".to_string(),
                args_preview: "{}".to_string(),
                status: ToolExecutionStatus::Running,
                started_at: 1,
                finished_at: None,
                duration_ms: None,
                detail_ref: "subagent/subagent-1/call-0".to_string(),
            };
            let _removed = state.finish(&main);
            assert_eq!(
                state.observe(ToolExecutionProjectionUpdate {
                    kind: ToolExecutionProjectionKind::Started,
                    agent: "reviewer".to_string(),
                    summary: subagent_summary,
                }),
                ChannelToolObserveOutcome::Accepted
            );
            let subagent = ChannelToolAddress::subagent(
                "workspace-a",
                "conversation-a",
                "run-1",
                "subagent-1",
                "call-0",
            );
            assert!(state.entry(&subagent).is_some());
            assert!(state.entry(&main).is_none());
            let _removed = state.finish(&subagent);
            assert!(state.entry(&subagent).is_none());
            assert!(state.recent_terminals.len() <= super::super::CHANNEL_RECENT_TOOL_TERMINALS);

            let mut collisions = ChannelToolRenderState::default();
            let first = match projection_event(
                "collision",
                "read_file",
                "{}",
                ToolExecutionStatus::Running,
                ToolExecutionProjectionKind::Started,
            ) {
                ChannelRenderEvent::ToolProjection(update) => update,
                _ => return Err("fixture did not produce first collision projection".to_string()),
            };
            let original_summary = first.summary.clone();
            let original_address = ChannelToolAddress::from_summary(&original_summary);
            assert_eq!(
                collisions.observe(first),
                ChannelToolObserveOutcome::Accepted
            );

            let independent = ToolExecutionSummary {
                id: "independent-canonical-id".to_string(),
                call_id: "independent".to_string(),
                detail_ref: "chat/message-1/independent".to_string(),
                ..original_summary.clone()
            };
            let independent_address = ChannelToolAddress::from_summary(&independent);
            assert_eq!(
                collisions.observe(ToolExecutionProjectionUpdate {
                    kind: ToolExecutionProjectionKind::Started,
                    agent: "echo-assistant".to_string(),
                    summary: independent,
                }),
                ChannelToolObserveOutcome::Accepted
            );

            let mut same_address_other_id = original_summary.clone();
            same_address_other_id.id = "other-canonical-id".to_string();
            assert_eq!(
                collisions.observe(ToolExecutionProjectionUpdate {
                    kind: ToolExecutionProjectionKind::Started,
                    agent: "echo-assistant".to_string(),
                    summary: same_address_other_id,
                }),
                ChannelToolObserveOutcome::IdentityConflict
            );
            assert!(collisions.entry(&original_address).is_none());
            assert!(collisions.detail_ref(&original_address).is_none());
            assert!(matches!(
                collisions.finish(&original_address),
                ChannelToolTerminal::IdentityConflict
            ));
            assert!(collisions.entry(&independent_address).is_some());
            assert!(matches!(
                collisions.finish(&independent_address),
                ChannelToolTerminal::Render(Some(_))
            ));

            let mut canonical_collision = ChannelToolRenderState::default();
            let first = ToolExecutionProjectionUpdate {
                kind: ToolExecutionProjectionKind::Started,
                agent: "echo-assistant".to_string(),
                summary: original_summary.clone(),
            };
            assert_eq!(
                canonical_collision.observe(first),
                ChannelToolObserveOutcome::Accepted
            );
            let mut same_id_other_address = original_summary.clone();
            same_id_other_address.workspace_id = "workspace-b".to_string();
            same_id_other_address.detail_ref = "workspace-b/collision".to_string();
            let other_address = ChannelToolAddress::from_summary(&same_id_other_address);
            assert_eq!(
                canonical_collision.observe(ToolExecutionProjectionUpdate {
                    kind: ToolExecutionProjectionKind::Finished,
                    agent: "echo-assistant".to_string(),
                    summary: same_id_other_address,
                }),
                ChannelToolObserveOutcome::IdentityConflict
            );
            assert!(canonical_collision.entry(&original_address).is_none());
            assert!(canonical_collision.entry(&other_address).is_none());
            assert!(matches!(
                canonical_collision.finish(&other_address),
                ChannelToolTerminal::IdentityConflict
            ));

            let mut terminal_canonical_collision = ChannelToolRenderState::default();
            assert_eq!(
                terminal_canonical_collision.observe(ToolExecutionProjectionUpdate {
                    kind: ToolExecutionProjectionKind::Started,
                    agent: "echo-assistant".to_string(),
                    summary: original_summary.clone(),
                }),
                ChannelToolObserveOutcome::Accepted
            );
            assert!(matches!(
                terminal_canonical_collision.finish(&original_address),
                ChannelToolTerminal::Render(Some(_))
            ));
            let mut terminal_same_id_other_address = original_summary.clone();
            terminal_same_id_other_address.workspace_id = "workspace-after-terminal".to_string();
            terminal_same_id_other_address.detail_ref =
                "workspace-after-terminal/collision".to_string();
            let terminal_other_address =
                ChannelToolAddress::from_summary(&terminal_same_id_other_address);
            assert_eq!(
                terminal_canonical_collision.observe(ToolExecutionProjectionUpdate {
                    kind: ToolExecutionProjectionKind::Started,
                    agent: "echo-assistant".to_string(),
                    summary: terminal_same_id_other_address,
                }),
                ChannelToolObserveOutcome::IdentityConflict
            );
            assert!(
                terminal_canonical_collision
                    .entry(&original_address)
                    .is_none()
            );
            assert!(
                terminal_canonical_collision
                    .entry(&terminal_other_address)
                    .is_none()
            );
            assert!(matches!(
                terminal_canonical_collision.finish(&terminal_other_address),
                ChannelToolTerminal::IdentityConflict
            ));

            let mut replay = ChannelToolRenderState::default();
            assert_eq!(
                replay.observe(ToolExecutionProjectionUpdate {
                    kind: ToolExecutionProjectionKind::Started,
                    agent: "echo-assistant".to_string(),
                    summary: original_summary.clone(),
                }),
                ChannelToolObserveOutcome::Accepted
            );
            assert!(matches!(
                replay.finish(&original_address),
                ChannelToolTerminal::Render(Some(_))
            ));
            assert_eq!(
                replay.observe(ToolExecutionProjectionUpdate {
                    kind: ToolExecutionProjectionKind::Started,
                    agent: "echo-assistant".to_string(),
                    summary: original_summary.clone(),
                }),
                ChannelToolObserveOutcome::Duplicate
            );
            assert!(matches!(
                replay.finish(&original_address),
                ChannelToolTerminal::Duplicate
            ));
            let mut replayed_different_id = original_summary;
            replayed_different_id.id = "replayed-different-id".to_string();
            assert_eq!(
                replay.observe(ToolExecutionProjectionUpdate {
                    kind: ToolExecutionProjectionKind::Started,
                    agent: "echo-assistant".to_string(),
                    summary: replayed_different_id,
                }),
                ChannelToolObserveOutcome::IdentityConflict
            );
            assert!(matches!(
                replay.finish(&original_address),
                ChannelToolTerminal::IdentityConflict
            ));
            Ok(())
        }

        #[tokio::test]
        async fn multibyte_no_panic() {
            // 原生中文与emoji保留，空final不制造第二份正文。
            let evs = events_to_stream(vec![
                Ok(super::native_commentary_block("你好🦀世界", 1)),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts, vec!["你好🦀世界".to_string()]);
        }

        #[tokio::test]
        async fn native_fullwidth_text_preserves_unicode() {
            // Native blocks preserve fullwidth punctuation and Unicode text.
            let evs = events_to_stream(vec![
                Ok(super::native_commentary_block("第一句！", 1)),
                Ok(super::native_commentary_block("第二句？", 2)),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts.concat(), "第一句！第二句？");
        }
    }
}

#[cfg(feature = "channels")]
fn channel_session_config(timeout_minutes: u64) -> echo_agent::channels::SessionConfig {
    echo_agent::channels::SessionConfig::default()
        .with_timeout_minutes(timeout_minutes)
        .with_reset_keywords(Vec::new())
        .with_command_prefix(None)
        .with_reset_commands(Vec::new())
}

#[cfg(test)]
mod original_composition_tests {
    use super::*;
    use anyhow::Result;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(feature = "channels")]
    struct ResetProbe {
        calls: Arc<AtomicUsize>,
    }

    #[cfg(feature = "channels")]
    struct SenderSessionProbe {
        generation: usize,
    }

    #[cfg(feature = "channels")]
    #[async_trait::async_trait]
    impl echo_agent::channels::MessageHandler for ResetProbe {
        async fn handle(
            &self,
            message: echo_agent::channels::InboundMessage,
        ) -> echo_agent::error::Result<echo_agent::channels::OutboundMessage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(echo_agent::channels::OutboundMessage::new(
                message.channel_id,
                message.chat_id,
                message.chat_type,
                "app-owned-reset",
            ))
        }

        async fn reply(
            &self,
            _message: echo_agent::channels::OutboundMessage,
        ) -> echo_agent::error::Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "channels")]
    #[async_trait::async_trait]
    impl echo_agent::channels::MessageHandler for SenderSessionProbe {
        async fn handle(
            &self,
            message: echo_agent::channels::InboundMessage,
        ) -> echo_agent::error::Result<echo_agent::channels::OutboundMessage> {
            Ok(echo_agent::channels::OutboundMessage::new(
                &message.channel_id,
                message.reply_target(),
                message.chat_type,
                self.generation.to_string(),
            ))
        }

        async fn reply(
            &self,
            _message: echo_agent::channels::OutboundMessage,
        ) -> echo_agent::error::Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn production_session_config_forwards_reset_to_app_handler() -> Result<()> {
        use echo_agent::channels::{ChatType, InboundMessage, MessageHandler, SessionHandler};
        use futures::StreamExt;

        let calls = Arc::new(AtomicUsize::new(0));
        let factory_calls = Arc::clone(&calls);
        let handler = SessionHandler::new(
            channel_session_config(30),
            move |_instance: &echo_agent::channels::ChannelSessionInstance| {
                Box::new(ResetProbe {
                    calls: Arc::clone(&factory_calls),
                }) as Box<dyn MessageHandler>
            },
        );
        let mut stream = handler
            .handle_stream(InboundMessage::new(
                "test-channel",
                "sender",
                "conversation",
                ChatType::Direct,
                "/reset",
                "message",
            ))
            .await?;
        let response = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("session handler returned an empty reset stream"))??;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(response.text, "app-owned-reset");
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn production_session_config_isolates_group_senders_and_reuses_each_sender() -> Result<()>
    {
        use echo_agent::channels::{ChatType, InboundMessage, MessageHandler, SessionHandler};

        let created = Arc::new(AtomicUsize::new(0));
        let factory_created = Arc::clone(&created);
        let handler = SessionHandler::new(
            channel_session_config(30),
            move |_instance: &echo_agent::channels::ChannelSessionInstance| {
                let generation = factory_created
                    .fetch_add(1, Ordering::SeqCst)
                    .saturating_add(1);
                Box::new(SenderSessionProbe { generation }) as Box<dyn MessageHandler>
            },
        );
        let message = |sender_id: &str, message_id: &str| {
            InboundMessage::new(
                "test-channel",
                sender_id,
                "shared-group",
                ChatType::Group,
                "hello",
                message_id,
            )
        };

        let alice_first = handler.handle(message("alice", "m1")).await?;
        let bob = handler.handle(message("bob", "m2")).await?;
        let alice_second = handler.handle(message("alice", "m3")).await?;
        assert_eq!(alice_first.text, "1");
        assert_eq!(bob.text, "2");
        assert_eq!(alice_second.text, "1");
        assert_eq!(handler.active_sessions(), 2);
        assert_eq!(created.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[cfg(feature = "channels")]
    struct LifecycleProbeChannel {
        id: &'static str,
        fail_start: bool,
        fail_stop: bool,
        stops: Arc<AtomicUsize>,
    }

    #[cfg(feature = "channels")]
    fn channel_test_error(message: impl Into<String>) -> echo_agent::error::ReactError {
        echo_agent::error::ReactError::Channel(Box::new(echo_agent::error::ChannelError::Other(
            message.into(),
        )))
    }

    #[cfg(feature = "channels")]
    static LIFECYCLE_PROBE_CAPABILITIES: echo_agent::channels::ChannelCapabilities =
        echo_agent::channels::ChannelCapabilities {
            chat_types: &[echo_agent::channels::ChatType::Direct],
            supports_media: false,
            supports_threads: false,
        };

    #[cfg(feature = "channels")]
    #[async_trait::async_trait]
    impl echo_agent::channels::ChannelPlugin for LifecycleProbeChannel {
        fn id(&self) -> &str {
            self.id
        }

        fn capabilities(&self) -> &echo_agent::channels::ChannelCapabilities {
            &LIFECYCLE_PROBE_CAPABILITIES
        }

        async fn start(
            &mut self,
            _handler: Arc<dyn echo_agent::channels::MessageHandler>,
        ) -> echo_agent::error::Result<()> {
            if self.fail_start {
                Err(channel_test_error(format!("{} start failure", self.id)))
            } else {
                Ok(())
            }
        }

        async fn stop(&mut self) -> echo_agent::error::Result<()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            if self.fail_stop {
                Err(channel_test_error(format!("{} stop failure", self.id)))
            } else {
                Ok(())
            }
        }

        async fn send(
            &self,
            _message: echo_agent::channels::OutboundMessage,
        ) -> echo_agent::error::Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn real_channel_manager_drains_and_aggregates_start_and_stop_failures() -> Result<()> {
        use echo_agent::channels::{ChannelManager, MessageHandler};

        let start_stops = Arc::new(AtomicUsize::new(0));
        let stop_stops = Arc::new(AtomicUsize::new(0));
        let mut manager = ChannelManager::new();
        manager.register(Box::new(LifecycleProbeChannel {
            id: "start-fails",
            fail_start: true,
            fail_stop: false,
            stops: Arc::clone(&start_stops),
        }))?;
        manager.register(Box::new(LifecycleProbeChannel {
            id: "stop-fails",
            fail_start: false,
            fail_stop: true,
            stops: Arc::clone(&stop_stops),
        }))?;

        let starts = manager
            .start_all(|_| {
                Arc::new(ResetProbe {
                    calls: Arc::new(AtomicUsize::new(0)),
                }) as Arc<dyn MessageHandler>
            })
            .await;
        let start_failures = starts
            .into_iter()
            .filter_map(|result| {
                result
                    .result
                    .err()
                    .map(|error| format!("{}: {error}", result.channel_id))
            })
            .collect::<Vec<_>>();
        let stop_result = manager.stop_all().await.map_err(anyhow::Error::from);
        assert!(
            start_failures
                .iter()
                .any(|message| message.contains("start-fails"))
        );
        let error = stop_result
            .err()
            .ok_or_else(|| anyhow::anyhow!("stop failure was not reported"))?;

        assert_eq!(start_stops.load(Ordering::SeqCst), 1);
        assert_eq!(stop_stops.load(Ordering::SeqCst), 1);
        assert!(error.to_string().contains("stop-fails"));
        Ok(())
    }
}
