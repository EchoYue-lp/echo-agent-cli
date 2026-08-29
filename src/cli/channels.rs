//! 应用层 IM channel 消息处理器 —— 把 IM 消息桥接到 `AgentPool`。
//!
//! 框架层 `AgentChannelHandler::from_config` 要求调用方显式传入 `LlmConfig`，而 EKO
//! channel 不直接构造该 handler。此处 agent 从 `AgentPool::acquire` 取，经
//! `AgentRuntime::bootstrap` 全套接通
//! （state_store / store / compressor / MemoryLayerManager / permission_service /
//! cache_user_id / conversation_id）。会话身份与 framework `SessionHandler` 一致，按
//! channel + conversation + sender 隔离；群聊成员不会交叉复用 Agent、TaskRun 或缓存。
//!
//! 归属（spec §D1-6）：`AgentPool` 是 EKO 产品概念，handler 放应用层（bin crate），
//! 不进框架 `channels.rs`。框架复用方可按需使用要求显式 LLM 依赖的
//! `AgentChannelHandler::from_config` / `from_config_with_client`。

#[cfg(feature = "channels")]
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::Arc,
};

#[cfg(feature = "channels")]
use echo_agent_app_core::foreground_turn::{
    ForegroundTurnControl, ForegroundTurnError, ForegroundTurnSurface,
};

#[cfg(feature = "channels")]
use echo_agent_app_core::hitl::{ChannelHumanLoopProvider, ChannelHumanLoopResolution};

#[cfg(feature = "channels")]
use sha2::{Digest, Sha256};

#[cfg(feature = "channels")]
enum ChannelRenderEvent {
    Token(String),
    Driver(echo_agent_app_core::chat_driver::ChatDriverEvent),
    Journaled(echo_agent_app_core::chat_event_log::ChatEventEnvelope),
    ToolProjection(echo_agent_app_core::tool_execution_projection::ToolExecutionProjectionUpdate),
    Prompt(String),
    Terminal(echo_agent_app_core::chat_driver::TurnOutcome),
}

#[cfg(feature = "channels")]
mod outbound;

#[cfg(feature = "channels")]
mod input_pump;

#[cfg(feature = "channels")]
use outbound::{
    CHANNEL_EVENT_QUEUE_CAPACITY, ChannelOutboundDraft, ChannelSurfaceSink,
    channel_outbound_chunks, channel_outbound_transport, channel_safe_text,
    channel_terminal_stream, immediate_channel_response,
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
    CHANNEL_OUTBOUND_TOTAL_MESSAGES, CHANNEL_TOKEN_COALESCE_BYTES, ChannelBufferOutcome,
    ChannelStreamingSanitizer, channel_outbound_transport_unpaced, channel_rate_deadline,
    channel_rate_policy,
};

#[cfg(all(feature = "channels", test))]
use tool_render::{
    CHANNEL_ACTIVE_TOOL_LIMIT, CHANNEL_RECENT_TOOL_TERMINALS, CHANNEL_TOOL_OUTPUT_CHARS,
};

#[cfg(feature = "channels")]
enum ChannelTaskRunControl {
    Reply(String),
    Resume {
        expected: echo_agent_app_core::tasks::task_runtime::TaskRunResumeIdentity,
        continuation_enabled: bool,
        runtime: Box<echo_agent_app_core::state::ScopedChatRuntime>,
    },
}

#[cfg(feature = "channels")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChannelResumeDispatch {
    Planned(echo_agent_app_core::tasks::task_runtime::TaskRunResumeIdentity),
    Continuation(echo_agent_app_core::tasks::task_runtime::RunTurnBinding),
}

#[cfg(feature = "channels")]
fn channel_resume_dispatch(
    expected: echo_agent_app_core::tasks::task_runtime::TaskRunResumeIdentity,
    continuation_enabled: bool,
    turn_id: &str,
) -> ChannelResumeDispatch {
    if continuation_enabled {
        ChannelResumeDispatch::Continuation(
            echo_agent_app_core::tasks::task_runtime::RunTurnBinding::resume_expected(
                expected, turn_id,
            ),
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
    runtime: echo_agent_app_core::state::ScopedChatRuntime,
    agent_conversation_id: String,
    conversation_id: String,
    turn_id: String,
}

#[cfg(feature = "channels")]
fn channel_input_address(
    workspace_id: &str,
    conversation_id: &str,
) -> echo_agent_app_core::conversation_input::ConversationInputAddress {
    echo_agent_app_core::conversation_input::ConversationInputAddress {
        workspace_id: workspace_id.to_string(),
        conversation_id: conversation_id.to_string(),
    }
}

#[cfg(feature = "channels")]
fn channel_input_attempt(
    projection: &echo_agent_app_core::conversation_input::ConversationInputProjection,
) -> Result<echo_agent_app_core::conversation_input::ConversationInputAttempt, String> {
    projection
        .active_attempt
        .clone()
        .ok_or_else(|| "channel input active attempt is missing".to_string())
}

#[cfg(feature = "channels")]
fn channel_input_phase_label(
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

#[cfg(feature = "channels")]
fn channel_input_fact_phase(
    fact: &echo_agent_app_core::conversation_input::ConversationInputFact,
) -> Option<echo_agent_app_core::conversation_input::ConversationInputPhase> {
    use echo_agent_app_core::conversation_input::{ConversationInputFact, ConversationInputPhase};

    match fact {
        ConversationInputFact::Persisted { .. } => Some(ConversationInputPhase::Persisted),
        ConversationInputFact::AttemptStarted { .. } => {
            Some(ConversationInputPhase::AttemptStarted)
        }
        ConversationInputFact::MailboxAccepted { .. } => {
            Some(ConversationInputPhase::MailboxAccepted)
        }
        ConversationInputFact::Drained { .. } => Some(ConversationInputPhase::Drained),
        ConversationInputFact::TurnSettled { .. } => Some(ConversationInputPhase::TurnSettled),
        ConversationInputFact::Deferred { .. } => Some(ConversationInputPhase::Deferred),
        ConversationInputFact::Reordered { .. } => None,
        ConversationInputFact::RecoveryRequired { .. } => {
            Some(ConversationInputPhase::RecoveryRequired)
        }
        ConversationInputFact::Cancelled { .. } => Some(ConversationInputPhase::Cancelled),
    }
}

#[cfg(feature = "channels")]
async fn settle_channel_turn_after_input_observers(
    lease: echo_agent_app_core::foreground_turn::ForegroundTurnLease,
    outcome: echo_agent_app_core::chat_driver::TurnOutcome,
) -> Result<echo_agent_app_core::chat_driver::TurnOutcome, String> {
    lease
        .settle_after_observers(outcome)
        .await
        .map(|settlement| settlement.outcome)
        .map_err(|error| error.to_string())
}

#[cfg(feature = "channels")]
fn channel_live_terminal_projector(
    service: echo_agent_app_core::conversation_input::ConversationInputService,
    attempt: echo_agent_app_core::conversation_input::ConversationInputAttempt,
    observed_phase: Arc<
        std::sync::Mutex<Option<echo_agent_app_core::conversation_input::ConversationInputPhase>>,
    >,
) -> echo_agent_app_core::foreground_turn::ForegroundTerminalProjector {
    use echo_agent_app_core::conversation_input::ConversationInputPhase;

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
async fn project_channel_input_lifecycle(
    log: &echo_agent_app_core::chat_event_log::ChatEventLog,
    identity: &echo_agent_app_core::conversation_input::ConversationInputIdentity,
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
            "channel input lifecycle replay for {} was truncated after cursor {}",
            identity.input_id, after_cursor
        ));
    }
    for envelope in replay.events {
        let sequence = envelope.sequence;
        if matches!(
            &envelope.payload,
            echo_agent_app_core::chat_driver::ChatDriverEvent::InputLifecycle(_)
        ) {
            render_tx
                .send(ChannelRenderEvent::Driver(envelope.payload))
                .await
                .map_err(|_| "channel input lifecycle renderer is closed".to_string())?;
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

#[cfg(feature = "channels")]
#[derive(Default)]
struct ChannelInputPumpTasks {
    shutting_down: bool,
    tasks: tokio::task::JoinSet<Result<(), String>>,
}

#[cfg(feature = "channels")]
#[derive(Default)]
pub(crate) struct ChannelSessionCoordinator {
    active_turns: ChannelActiveTurnMap,
    lifecycle: std::sync::Mutex<HashMap<ChannelSurfaceIdentity, ChannelSessionLifecycleRecord>>,
    input_pumps: std::sync::Mutex<
        HashMap<
            ChannelSurfaceIdentity,
            Arc<
                input_pump::ChannelInputPumpSlot<
                    echo_agent_app_core::conversation_input::ConversationInputIdentity,
                >,
            >,
        >,
    >,
    input_pump_tasks: std::sync::Mutex<ChannelInputPumpTasks>,
}

#[cfg(feature = "channels")]
impl ChannelSessionCoordinator {
    pub(crate) fn new() -> Self {
        Self::default()
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
    ) -> Arc<
        input_pump::ChannelInputPumpSlot<
            echo_agent_app_core::conversation_input::ConversationInputIdentity,
        >,
    > {
        Arc::clone(
            self.input_pumps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(surface_id.clone())
                .or_default(),
        )
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
        if ended_current
            && let Some(slot) = self
                .input_pumps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&surface_id)
        {
            let _ = slot.begin_shutdown();
        }
    }

    fn start_input_pump_task(
        &self,
        slot: Arc<
            input_pump::ChannelInputPumpSlot<
                echo_agent_app_core::conversation_input::ConversationInputIdentity,
            >,
        >,
        owner: input_pump::ChannelInputPumpOwner<
            echo_agent_app_core::conversation_input::ConversationInputIdentity,
        >,
        adapter: Arc<ChannelInputPumpAdapter>,
    ) -> Result<(), String> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| format!("channel input pump requires a Tokio runtime: {error}"))?;
        let mut tasks = self
            .input_pump_tasks
            .lock()
            .map_err(|_| "channel input pump task owner is unavailable".to_string())?;
        if tasks.shutting_down {
            return Err("channel input pump admission is closed".to_string());
        }
        tasks.tasks.spawn_on(
            async move {
                let mut owner = Some(owner);
                loop {
                    let current = owner
                        .take()
                        .ok_or_else(|| "channel pump recovery owner is missing".to_string())?;
                    match input_pump::run_channel_input_pump(current, Arc::clone(&adapter)).await {
                        Ok(()) | Err(input_pump::ChannelInputPumpError::ShuttingDown) => {
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
        waiter: echo_agent_app_core::foreground_turn::ForegroundTurnSettlementWaiter,
        terminal_tx: tokio::sync::oneshot::Sender<echo_agent_app_core::chat_driver::TurnOutcome>,
        slot: Arc<
            input_pump::ChannelInputPumpSlot<
                echo_agent_app_core::conversation_input::ConversationInputIdentity,
            >,
        >,
        adapter: Arc<ChannelInputPumpAdapter>,
    ) -> Result<(), String> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| format!("channel input notifier requires a Tokio runtime: {error}"))?;
        let mut tasks = self
            .input_pump_tasks
            .lock()
            .map_err(|_| "channel input pump task owner is unavailable".to_string())?;
        if tasks.shutting_down {
            return Err("channel input notifier admission is closed".to_string());
        }
        tasks.tasks.spawn_on(
            async move {
                let settlement = waiter.wait().await.map_err(|error| error.to_string())?;
                let _ = terminal_tx.send(settlement.outcome);
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
                            Ok(()) | Err(input_pump::ChannelInputPumpError::ShuttingDown) => break,
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

    pub(crate) async fn join_input_pumps(&self) -> Result<(), String> {
        let mut owned = {
            let mut tasks = self
                .input_pump_tasks
                .lock()
                .map_err(|_| "channel input pump task owner is unavailable".to_string())?;
            std::mem::take(&mut tasks.tasks)
        };
        let mut failures = Vec::new();
        while let Some(result) = owned.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error),
                Err(error) => failures.push(error.to_string()),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
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
    result: &Result<
        echo_agent_app_core::foreground_turn::ForegroundTurnSettlement,
        ForegroundTurnError,
    >,
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
) -> Result<echo_agent_app_core::foreground_turn::ForegroundTurnSettlement, ForegroundTurnError> {
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
    snapshot: &echo_agent_app_core::foreground_turn::ForegroundTurnSnapshot,
    root_turn_id: &str,
) -> bool {
    snapshot.root_turn_id == root_turn_id
}

#[cfg(feature = "channels")]
fn channel_snapshot_for_conversation(
    snapshots: Vec<echo_agent_app_core::foreground_turn::ForegroundTurnSnapshot>,
    conversation_id: &str,
) -> Result<Option<echo_agent_app_core::foreground_turn::ForegroundTurnSnapshot>, String> {
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
fn channel_steer_target(
    snapshot: &echo_agent_app_core::foreground_turn::ForegroundTurnSnapshot,
) -> &str {
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
) -> (echo_agent_app_core::chat_driver::TurnOutcome, String) {
    match error {
        ChannelSessionRetirementError::Cancelled => (
            echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
            "Channel session retirement was cancelled; retry the message.".to_string(),
        ),
        ChannelSessionRetirementError::Failed(error) => (
            echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(
                    "channel_session_generation",
                    error.clone(),
                ),
            ),
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
    mut terminal_rx: tokio::sync::oneshot::Receiver<echo_agent_app_core::chat_driver::TurnOutcome>,
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
                    None => driver_open = false,
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
                    terminal_outcome = Some(terminal.unwrap_or_else(|_| {
                        echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                            echo_agent::error::AgentFailure::message(
                                "foreground_supervisor",
                                "channel foreground owner ended without a terminal receipt",
                            ),
                        )
                    }));
                }
            }
            // The single foreground owner spans every finite continuation turn.
            // Publish its terminal receipt only after the renderer channel has
            // closed, preserving all driver events and HITL prompts before Done.
            if !driver_open
                && let Some(outcome) = terminal_outcome.take()
            {
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
    tool_executions: Arc<echo_agent_app_core::tool_execution::ToolExecutionRepository>,
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
    )
    .await
}

/// IM channel 消息处理器：每 `handle` 从 AppState 的 exact scoped runtime 取/复用 per-sender agent。
///
/// TUI/GUI functional parity (AGENTS.md): channels drive chat through the
/// shared foreground driver. TaskRuntime and AgentPool ownership are resolved
/// from the captured AppState workspace generation.
/// Whether a complex run is warranted is decided by the agent itself, not
/// pre-judged here.
#[cfg(feature = "channels")]
pub struct AppChannelMessageHandler {
    app_state: Arc<echo_agent_app_core::state::AppState>,
    webhook_emitter: Arc<echo_agent_app_core::webhook::WebhookEmitter>,
    hitl: Arc<ChannelHumanLoopProvider>,
    foreground_turns: ForegroundTurnControl,
    session_instance: echo_agent::channels::ChannelSessionInstance,
    session_coordinator: Arc<ChannelSessionCoordinator>,
    initialization_error: Option<String>,
    incarnation_fault: Arc<std::sync::Mutex<Option<String>>>,
    active_turns: ChannelActiveTurnMap,
    input_pump: Arc<
        input_pump::ChannelInputPumpSlot<
            echo_agent_app_core::conversation_input::ConversationInputIdentity,
        >,
    >,
    pending_retirement: ChannelSessionRetirementGate,
}

#[cfg(feature = "channels")]
struct ChannelInputPumpItem {
    projection: echo_agent_app_core::conversation_input::ConversationInputProjection,
    attempt: echo_agent_app_core::conversation_input::ConversationInputAttempt,
    runtime: echo_agent_app_core::state::ScopedChatRuntime,
    lease: std::sync::Mutex<Option<echo_agent_app_core::foreground_turn::ForegroundTurnLease>>,
}

#[cfg(feature = "channels")]
struct ChannelInputPumpAdapter {
    app_state: Arc<echo_agent_app_core::state::AppState>,
    foreground_turns: ForegroundTurnControl,
    address: echo_agent_app_core::conversation_input::ConversationInputAddress,
    agent_conversation_id: String,
    cache_id: String,
    hitl: Arc<ChannelHumanLoopProvider>,
}

#[cfg(feature = "channels")]
enum ChannelLiveInputRoute {
    Routed,
    PumpPending(input_pump::ChannelInputReplyRoute),
}

#[cfg(feature = "channels")]
impl input_pump::ChannelInputPumpAdapter for ChannelInputPumpAdapter {
    type Identity = echo_agent_app_core::conversation_input::ConversationInputIdentity;
    type Item = ChannelInputPumpItem;

    fn peek_next_identity(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<Option<Self::Identity>, String>> {
        Box::pin(async move {
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
                .list(&self.address)
                .await
                .map_err(|error| error.to_string())?;
            let Some(next) = frontier.items.first() else {
                return Ok(None);
            };
            if next.receipt.identity != *expected_identity {
                return Ok(None);
            }
            let runtime = self
                .app_state
                .chat_runtime_for_scope(&self.address.workspace_id)
                .await
                .map_err(|error| error.to_string())?;
            if runtime.execution_scope().workspace_id() != self.address.workspace_id {
                return Err("channel input workspace changed before claim".to_string());
            }
            let turn_id = format!("channel-input-turn:{}", expected_identity.input_id);
            let lease = match runtime
                .begin_turn(
                    &self.foreground_turns,
                    ForegroundTurnSurface::Channel,
                    &self.address.conversation_id,
                    turn_id.clone(),
                )
                .await
            {
                Ok(lease) => lease,
                Err(echo_agent_app_core::conversation_deletion::ConversationDeletionError::Foreground(
                    ForegroundTurnError::Busy { .. },
                )) => {
                    return Err("channel foreground is busy; pump will retry".to_string());
                }
                Err(error) => return Err(error.to_string()),
            };
            let projection = match self
                .app_state
                .conversation_inputs()
                .dispatch_selected(expected_identity.clone(), frontier.queue_revision, turn_id)
                .await
            {
                Ok(projection) => projection,
                Err(error) => {
                    settle_channel_turn_after_input_observers(
                        lease,
                        echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                    )
                    .await
                    .map_err(|settle| format!("{error}; foreground settlement failed: {settle}"))?;
                    return Err(error.to_string());
                }
            };
            let attempt = match channel_input_attempt(&projection) {
                Ok(attempt) => attempt,
                Err(error) => {
                    let failure = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
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
            Ok(Some(ChannelInputPumpItem {
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
            let Some(pool) = item.runtime.pool() else {
                return Err(input_pump::ChannelInputExecutionError::before_driver(
                    "channel input runtime has no AgentPool",
                    reply,
                ));
            };
            let execution = match pool.acquire(&self.agent_conversation_id).await {
                Ok(execution) => execution,
                Err(error) => {
                    return Err(input_pump::ChannelInputExecutionError::before_driver(
                        error.to_string(),
                        reply,
                    ));
                }
            };
            let agent = execution.agent();
            configure_channel_agent(&agent, &self.cache_id, Arc::clone(&self.hitl)).await;
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
                    text: item.projection.payload.text.clone(),
                    conversation_id: self.address.conversation_id.clone(),
                    turn_id: item.attempt.turn_id.clone(),
                    runtime_authored: false,
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
            let lifecycle_render_tx = reply.render_tx.clone();
            let lifecycle_terminal_tx = reply.render_tx.clone();
            let lifecycle_cursor = Arc::clone(&reply.lifecycle_cursor);
            let terminal_lifecycle_cursor = Arc::clone(&reply.lifecycle_cursor);
            let lifecycle_log = Arc::clone(&self.app_state.storage.chat_events);
            let terminal_lifecycle_log = Arc::clone(&self.app_state.storage.chat_events);
            let lifecycle_identity = item.attempt.identity.clone();
            let terminal_lifecycle_identity = item.attempt.identity.clone();
            let inner_sink: Arc<dyn echo_agent_app_core::chat_driver::ChatSink> =
                Arc::new(ChannelSurfaceSink::new(reply.render_tx, cancel.clone()));
            let terminal_tx = reply.terminal_tx;
            let sink = echo_agent_app_core::chat_event_log::bind_surface_chat_sink(
                echo_agent_app_core::chat_event_log::ChatSurface::Channel,
                inner_sink,
                self.app_state.storage.chat_events.clone(),
                self.app_state.storage.tool_executions.clone(),
                self.address.workspace_id.clone(),
                Some(self.address.conversation_id.clone()),
                item.attempt.turn_id.clone(),
            );
            let resources = Arc::new(echo_agent_app_core::chat_resources::ChatResources {
                execution_scope: item.runtime.execution_scope().clone(),
                workspace_io_receipt: Some(item.runtime.workspace_io_receipt()),
                pool: Some(pool),
                store: item.runtime.task_runtime(),
                sink,
                webhook_emitter: None,
                conv_id: Some(self.address.conversation_id.clone()),
                root_message_id: item.attempt.turn_id.clone(),
                attachments: turn.inline_attachment_refs(),
                cancel,
                review_integration: item.runtime.review_integration(),
                memory_generation: None,
                human_loop_provider: Some(self.hitl.clone()),
            });
            let observer_service = self.app_state.conversation_inputs();
            let observer_attempt = item.attempt.clone();
            let observer: echo_agent_app_core::chat_driver::InputReceiptObserver = Arc::new(
                move |receipt| {
                    let service = observer_service.clone();
                    let attempt = observer_attempt.clone();
                    let render_tx = lifecycle_render_tx.clone();
                    let cursor = Arc::clone(&lifecycle_cursor);
                    let log = Arc::clone(&lifecycle_log);
                    let identity = lifecycle_identity.clone();
                    Box::pin(async move {
                        let observed = service
                            .observe_turn_input_through_drain(attempt, receipt)
                            .await;
                        let projected = project_channel_input_lifecycle(
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
                },
            );
            let terminal_service = self.app_state.conversation_inputs();
            let terminal_attempt = item.attempt.clone();
            let outcome = echo_agent_app_core::foreground_turn::drive_foreground_chat_with_ingress(
                lease,
                &agent,
                &turn,
                resources,
                observer,
                move |outcome| {
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
                        if let Err(error) = project_channel_input_lifecycle(
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
                },
            )
            .await
            .map_err(input_pump::ChannelInputExecutionError::after_driver)?;
            drop(execution);
            let _ = terminal_tx.send(outcome);
            Ok(())
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
            lease
                .settle_after(
                    echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                    move |_outcome| {
                        let service = service.clone();
                        let attempt = attempt.clone();
                        let reason = reason.clone();
                        async move {
                            service
                                .deferred(attempt, reason)
                                .await
                                .map(|_| ())
                                .map_err(|error| error.to_string())
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
        address: echo_agent_app_core::conversation_input::ConversationInputAddress,
        agent_conversation_id: String,
        cache_id: String,
    ) -> Result<(), String> {
        match self.input_pump.kick().map_err(|error| error.to_string())? {
            input_pump::ChannelInputPumpKick::Notified => Ok(()),
            input_pump::ChannelInputPumpKick::Started(owner) => {
                let adapter = Arc::new(ChannelInputPumpAdapter {
                    app_state: Arc::clone(&self.app_state),
                    foreground_turns: self.foreground_turns.clone(),
                    address,
                    agent_conversation_id,
                    cache_id,
                    hitl: Arc::clone(&self.hitl),
                });
                self.session_coordinator.start_input_pump_task(
                    Arc::clone(&self.input_pump),
                    owner,
                    adapter,
                )
            }
        }
    }

    pub(crate) fn new(
        app_state: Arc<echo_agent_app_core::state::AppState>,
        webhook_emitter: Arc<echo_agent_app_core::webhook::WebhookEmitter>,
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
        Self {
            app_state,
            webhook_emitter,
            hitl: Arc::new(ChannelHumanLoopProvider::new()),
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
            .app_state
            .current_control_runtime()
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
        submitted: echo_agent_app_core::conversation_input::ConversationInputReceipt,
        reply: input_pump::ChannelInputReplyRoute,
    ) -> Result<ChannelLiveInputRoute, String> {
        use echo_agent_app_core::conversation_input::ConversationInputPhase;

        let Some(active) = self
            .resolve_active_turn(active_surface_id, conv, agent_conv)
            .await?
        else {
            return Ok(ChannelLiveInputRoute::PumpPending(reply));
        };
        let workspace_id = active.runtime.execution_scope().workspace_id().to_string();
        if submitted.identity.address.workspace_id != workspace_id {
            return Ok(ChannelLiveInputRoute::PumpPending(reply));
        }
        let snapshots = self
            .foreground_turns
            .snapshots_for_workspace(&workspace_id)
            .map_err(|error| error.to_string())?;
        let Some(snapshot) = channel_snapshot_for_conversation(snapshots, conv)? else {
            Self::clear_active_turn(&self.active_turns, active_surface_id, &active.turn_id);
            return Ok(ChannelLiveInputRoute::PumpPending(reply));
        };
        if !channel_snapshot_matches_root(&snapshot, &active.turn_id) {
            Self::clear_active_turn(&self.active_turns, active_surface_id, &active.turn_id);
            return Ok(ChannelLiveInputRoute::PumpPending(reply));
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
            return Ok(ChannelLiveInputRoute::PumpPending(reply));
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
            Err(_) => return Ok(ChannelLiveInputRoute::PumpPending(reply)),
        };
        let attempt = match channel_input_attempt(&projection) {
            Ok(attempt) => attempt,
            Err(error) => {
                let failure = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message("conversation_input", error.clone()),
                );
                let _ = reply
                    .render_tx
                    .send(ChannelRenderEvent::Token(format!(
                        "Channel input {} failed before delivery: {error}",
                        submitted.identity.input_id
                    )))
                    .await;
                let _ = reply.terminal_tx.send(failure);
                return Ok(ChannelLiveInputRoute::Routed);
            }
        };
        let dispatched_input_id = projection.receipt.identity.input_id.clone();
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
                return Ok(ChannelLiveInputRoute::PumpPending(reply));
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
                return Ok(ChannelLiveInputRoute::PumpPending(reply));
            }
        };
        let turn = match prepare_channel_turn(
            ChannelTurnPreparation {
                attachments: projection.payload.attachments,
                execution_root: active.runtime.execution_scope().root().to_path_buf(),
                text: projection.payload.text,
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
                return Ok(ChannelLiveInputRoute::PumpPending(reply));
            }
        };
        let Some(pool) = active.runtime.pool() else {
            service
                .deferred(attempt, "active workspace has no AgentPool".to_string())
                .await
                .map_err(|error| error.to_string())?;
            return Ok(ChannelLiveInputRoute::PumpPending(reply));
        };
        let execution = match pool.lease_existing(&active.agent_conversation_id).await {
            Ok(Some(execution)) => execution,
            Ok(None) => {
                service
                    .deferred(attempt, "active channel Agent is unavailable".to_string())
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(ChannelLiveInputRoute::PumpPending(reply));
            }
            Err(error) => {
                service
                    .deferred(attempt, error.to_string())
                    .await
                    .map_err(|settle| settle.to_string())?;
                return Ok(ChannelLiveInputRoute::PumpPending(reply));
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
                return Ok(ChannelLiveInputRoute::PumpPending(reply));
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
            app_state: Arc::clone(&self.app_state),
            foreground_turns: self.foreground_turns.clone(),
            address: submitted.identity.address.clone(),
            agent_conversation_id: agent_conv.to_string(),
            cache_id: Self::cache_user_id(conv, &self.session_instance.incarnation_id()),
            hitl: Arc::clone(&self.hitl),
        });
        let terminal_projector = channel_live_terminal_projector(
            service.clone(),
            attempt.clone(),
            Arc::clone(&observed_phase),
        );
        if let Err(error) = self.foreground_turns.supervise_input_lifecycle_scoped(
            &workspace_id,
            ForegroundTurnSurface::Channel,
            conv,
            &active_agent_turn_id,
            async move {
                let steer = steer_rx.await.map_err(|error| error.to_string())?;
                let observed = match observer_service
                    .observe_steer_through_drain(observer_attempt, steer)
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
                    .send(ChannelRenderEvent::Token(format!(
                        "Channel input {} is {}.",
                        dispatched_input_id,
                        channel_input_phase_label(observed.phase)
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
            return Ok(ChannelLiveInputRoute::PumpPending(reply));
        }
        let steer = agent
            .steer_input_tracked(Some(&active_agent_turn_id), message)
            .await;
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
            ConversationInputPhase::Deferred => Ok(ChannelLiveInputRoute::PumpPending(reply)),
            ConversationInputPhase::RecoveryRequired => {
                let failure = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "channel_input_recovery",
                        observed.reason.unwrap_or_else(|| {
                            "channel input requires explicit recovery".to_string()
                        }),
                    ),
                );
                let _ = reply.terminal_tx.send(failure);
                Ok(ChannelLiveInputRoute::Routed)
            }
            ConversationInputPhase::Cancelled => {
                let _ = reply
                    .terminal_tx
                    .send(echo_agent_app_core::chat_driver::TurnOutcome::Cancelled);
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
                let failure = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "channel_input_recovery",
                        "channel input did not reach a safe replay boundary",
                    ),
                );
                let _ = reply.terminal_tx.send(failure);
                Ok(ChannelLiveInputRoute::Routed)
            }
        }
    }

    fn publish_active_turn(
        &self,
        surface_id: &ChannelSurfaceIdentity,
        conversation_id: &str,
        agent_conversation_id: &str,
        runtime: &echo_agent_app_core::state::ScopedChatRuntime,
        turn_id: &str,
    ) -> Result<(), String> {
        let mut active = self
            .active_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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

    async fn settle_pending_retirement_for_runtime(
        &self,
        runtime: &echo_agent_app_core::state::ScopedChatRuntime,
        conversation_id: &str,
        agent_conversation_id: &str,
        surface_id: &ChannelSurfaceIdentity,
    ) -> Result<(), String> {
        let obligations = self.pending_runtime_obligations();
        if obligations.is_empty() {
            return Ok(());
        }
        if let Some(active) = self.active_turn(surface_id) {
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
            Self::clear_active_turn(&self.active_turns, surface_id, &active.turn_id);
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
                echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
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
                Self::clear_runtime_incarnation(
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
                    echo_agent_app_core::chat_driver::TurnOutcome::Completed,
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
        runtime: &echo_agent_app_core::state::ScopedChatRuntime,
        product_conversation_id: &str,
        runtime_state_id: &str,
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
            incarnation_id: self.session_instance.incarnation_id(),
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
    ) -> Result<echo_agent_app_core::state::ScopedChatRuntime, String> {
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

    async fn clear_runtime_incarnation(
        runtime: &echo_agent_app_core::state::ScopedChatRuntime,
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
        echo_agent::state::clear_persisted_runtime_incarnation(
            conversation_store.as_ref(),
            runtime_state_store.as_ref(),
            conversation_id,
            runtime_state_id,
        )
        .await
        .map(|receipt| receipt.checkpoint_removed)
        .map_err(|error| error.to_string())
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
        runtime: &echo_agent_app_core::state::ScopedChatRuntime,
    ) -> Result<echo_agent_app_core::foreground_turn::ForegroundExecutionReceipts, String> {
        let task_generation = runtime
            .task_runtime()
            .as_ref()
            .map(|store| store.lease_foreground_generation())
            .transpose()
            .map_err(|error| format!("TaskRuntime generation is unavailable: {error}"))?;
        let mut receipts =
            echo_agent_app_core::foreground_turn::ForegroundExecutionReceipts::new(task_generation);
        if let Some(integration) = runtime.review_integration().as_ref() {
            let memory_generation = integration
                .lease_generation()
                .map_err(|error| format!("Memory generation is unavailable: {error}"))?;
            receipts.retain(memory_generation);
        }
        Ok(receipts)
    }

    async fn current_task_run(
        store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
        conv: &str,
        requested_run_id: Option<&str>,
    ) -> Result<
        (
            Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
            echo_agent_app_core::tasks::task_runtime::RunStateSnapshot,
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
                echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(
                    "No TaskRun was found for this conversation.".to_string(),
                )
            })?;
            if run.conversation_id != conversation_id {
                return Err(
                    echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(format!(
                        "TaskRun {} belongs to another conversation.",
                        run.run_id
                    )),
                );
            }
            store.get_run_state(&run.run_id)?.ok_or_else(|| {
                echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(format!(
                    "TaskRun {} has no event projection.",
                    run.run_id
                ))
            })
        })
        .await?;
        Ok((store, snapshot))
    }

    async fn task_runtime_io<T, F>(
        store: Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
        operation: &'static str,
        function: F,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(
                Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
            ) -> Result<T, echo_agent_app_core::tasks::task_runtime::StoreError>
            + Send
            + 'static,
    {
        echo_agent_app_core::tasks::task_runtime::TaskRuntimeBlockingAdapter::new(store)
            .run_store(operation, function)
            .await
            .map_err(|error| error.to_string())
    }

    async fn agent_router_command_response(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Option<String> {
        let trimmed = message.trim();
        let (command, arguments) = trimmed
            .split_once(char::is_whitespace)
            .unwrap_or((trimmed, ""));
        match command {
            "/agent-list" => Some(
                crate::cli::cmd_impls::agent_router::list_agent_endpoints(Some(&self.app_state))
                    .await,
            ),
            "/agent-send" => {
                let mut parts = arguments.splitn(3, char::is_whitespace);
                let (Some(workspace_id), Some(target_conversation_id), Some(text)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    return Some(
                        "Usage: /agent-send <workspace-id> <conversation-id> <message>".to_string(),
                    );
                };
                if text.trim().is_empty() {
                    return Some(
                        "Usage: /agent-send <workspace-id> <conversation-id> <message>".to_string(),
                    );
                }
                let from = match self
                    .app_state
                    .current_agent_address(Some(conversation_id))
                    .await
                {
                    Ok(address) => address,
                    Err(error) => {
                        return Some(format!("Agent source resolution failed: {error}"));
                    }
                };
                Some(
                    crate::cli::cmd_impls::agent_router::send_agent_text(
                        Some(&self.app_state),
                        from,
                        workspace_id,
                        target_conversation_id,
                        text,
                    )
                    .await,
                )
            }
            "/agent-status" => {
                let mut parts = arguments.split_whitespace();
                let (Some(workspace_id), Some(target_conversation_id)) =
                    (parts.next(), parts.next())
                else {
                    return Some(
                        "Usage: /agent-status <workspace-id> <conversation-id> [message-id]"
                            .to_string(),
                    );
                };
                Some(
                    crate::cli::cmd_impls::agent_router::agent_delivery_status(
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
            }
            "/agent-group" => {
                let args = arguments.split_whitespace().collect::<Vec<_>>();
                Some(
                    crate::cli::cmd_impls::agent_router::execute_agent_group_command(
                        Some(&self.app_state),
                        &args,
                    )
                    .await,
                )
            }
            _ => None,
        }
    }

    async fn task_run_command_response(
        &self,
        message: &str,
        conv: &str,
    ) -> Option<ChannelTaskRunControl> {
        let mut parts = message.split_whitespace();
        let command = parts.next()?;
        if !is_task_run_control_command(command) {
            return None;
        }
        let runtime = match self.app_state.current_control_runtime().await {
            Ok(runtime) => runtime,
            Err(error) => {
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
                "/subagent-message" => (crate::task_run_control::SUBAGENT_MESSAGE_USAGE, true),
                "/subagent-followup" => (crate::task_run_control::SUBAGENT_FOLLOWUP_USAGE, true),
                "/subagent-interrupt" => (crate::task_run_control::SUBAGENT_INTERRUPT_USAGE, false),
                _ => return None,
            };
            let values = parts.collect::<Vec<_>>();
            let parsed = match crate::task_run_control::parse_subagent_control_args(
                &values,
                usage,
                instruction_required,
            ) {
                Ok(parsed) => parsed,
                Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
            };
            let (store, _) = match Self::current_task_run(
                task_runtime.clone(),
                conv,
                Some(&parsed.identity.run_id),
            )
            .await
            {
                Ok(value) => value,
                Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
            };
            let service =
                echo_agent_app_core::tasks::task_runtime::SubagentControlService::new(store);
            let result = match command {
                "/subagent-message" => {
                    let Some(instruction) = parsed.instruction.as_deref() else {
                        return Some(ChannelTaskRunControl::Reply(format!("Usage: {usage}")));
                    };
                    service
                        .send_message(
                            parsed.identity,
                            instruction,
                            echo_agent_app_core::tasks::task_runtime::SubagentControlActorSource::Channel,
                        )
                        .await
                }
                "/subagent-followup" => {
                    let Some(instruction) = parsed.instruction.as_deref() else {
                        return Some(ChannelTaskRunControl::Reply(format!("Usage: {usage}")));
                    };
                    service
                        .queue_guidance_async(
                            parsed.identity,
                            instruction.to_string(),
                            echo_agent_app_core::tasks::task_runtime::SubagentControlActorSource::Channel,
                        )
                        .await
                }
                "/subagent-interrupt" => {
                    service
                        .interrupt_subagent(
                            parsed.identity,
                            echo_agent_app_core::tasks::task_runtime::SubagentControlActorSource::Channel,
                        )
                        .await
                }
                _ => return None,
            };
            let reply = match result {
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
            };
            return Some(ChannelTaskRunControl::Reply(reply));
        }
        if command == "/task-goal" {
            let values = parts.collect::<Vec<_>>();
            let parsed = match crate::task_run_control::parse_run_goal_update_args(&values) {
                Ok(parsed) => parsed,
                Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
            };
            let (store, snapshot) = match Self::current_task_run(
                task_runtime.clone(),
                conv,
                parsed.requested_run_id.as_deref(),
            )
            .await
            {
                Ok(value) => value,
                Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
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
                        echo_agent_app_core::tasks::task_runtime::RunGoalActorSource::Channel,
                    )
                },
            )
            .await
            {
                Ok(run) => format!(
                    "TaskRun {} Goal updated to revision {}; update its task graph before resuming.",
                    run.run_id, run.goal_revision
                ),
                Err(error) => format!("Unable to update TaskRun Goal: {error}"),
            };
            return Some(ChannelTaskRunControl::Reply(reply));
        }
        if command == "/task-requirements" {
            let requested_run_id = parts.next();
            let (store, snapshot) =
                match Self::current_task_run(task_runtime.clone(), conv, requested_run_id).await {
                    Ok(value) => value,
                    Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
                };
            let run_id = snapshot.run.run_id;
            let reply =
                match Self::task_runtime_io(store, "load channel completion gate", move |store| {
                    store.completion_gate_report(&run_id)
                })
                .await
                {
                    Ok(report) => format_channel_completion_gate(&report),
                    Err(error) => format!("Unable to read completion gate: {error}"),
                };
            return Some(ChannelTaskRunControl::Reply(reply));
        }
        if command == "/task-requirement-skip" {
            let values = parts.collect::<Vec<_>>();
            let parsed = match crate::task_run_control::parse_requirement_skip_args(&values) {
                Ok(parsed) => parsed,
                Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
            };
            let (store, snapshot) = match Self::current_task_run(
                task_runtime.clone(),
                conv,
                parsed.requested_run_id.as_deref(),
            )
            .await
            {
                Ok(value) => value,
                Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
            };
            let run_id = snapshot.run.run_id;
            let reply =
                match Self::task_runtime_io(store, "skip channel Goal requirement", move |store| {
                    store.skip_goal_requirement(
                        &run_id,
                        parsed.expected_goal_revision,
                        &parsed.requirement_id,
                        &parsed.reason,
                        echo_agent_app_core::tasks::task_runtime::RunGoalActorSource::Channel,
                    )
                })
                .await
                {
                    Ok(report) => format_channel_completion_gate(&report),
                    Err(error) => format!("Unable to confirm requirement Skip: {error}"),
                };
            return Some(ChannelTaskRunControl::Reply(reply));
        }
        let (action, budget_values, requested_run_id) = match command {
            "/task-run" => {
                let action = parts.next().unwrap_or("status");
                if !matches!(action, "status" | "pause" | "resume" | "cancel" | "budget") {
                    return Some(ChannelTaskRunControl::Reply(
                        "Usage: /task-run [status|pause|resume|cancel] [run-id], or /task-run budget <tokens|none> <seconds|none> [run-id]".to_string(),
                    ));
                }
                if action == "budget" {
                    let Some(tokens) = parts.next() else {
                        return Some(ChannelTaskRunControl::Reply(
                            "Usage: /task-run budget <tokens|none> <seconds|none> [run-id]"
                                .to_string(),
                        ));
                    };
                    let Some(time) = parts.next() else {
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
                    return Some(ChannelTaskRunControl::Reply(
                        "Usage: /task-budget <tokens|none> <seconds|none> [run-id]".to_string(),
                    ));
                };
                let Some(time) = parts.next() else {
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
                Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
            };
        let run_id = snapshot.run.run_id.clone();
        let reply = match action {
            "status" => format_channel_task_run_status(&snapshot),
            "pause" => {
                let owned_run_id = run_id.clone();
                match Self::task_runtime_io(store.clone(), "pause channel TaskRun", move |store| {
                    store.request_pause(&owned_run_id)
                })
                .await
                {
                    Ok(true) => format!("TaskRun {run_id} paused."),
                    Ok(false) => format!("TaskRun {run_id} is not actively pausable."),
                    Err(error) => format!("Unable to pause TaskRun {run_id}: {error}"),
                }
            }
            "cancel" => {
                let owned_run_id = run_id.clone();
                match Self::task_runtime_io(store.clone(), "cancel channel TaskRun", move |store| {
                    store.request_cancel(&owned_run_id)
                })
                .await
                {
                    Ok(true) => format!("TaskRun {run_id} cancelled."),
                    Ok(false) => format!("TaskRun {run_id} is already terminal."),
                    Err(error) => format!("Unable to cancel TaskRun {run_id}: {error}"),
                }
            }
            "resume" => {
                if snapshot.run.status
                    != echo_agent_app_core::tasks::task_runtime::TaskRunStatus::Paused
                {
                    format!(
                        "TaskRun {run_id} is {}; resume requires paused.",
                        snapshot.run.status.as_str()
                    )
                } else {
                    return Some(ChannelTaskRunControl::Resume {
                        expected:
                            echo_agent_app_core::tasks::task_runtime::TaskRunResumeIdentity::capture(
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
                    return Some(ChannelTaskRunControl::Reply(
                        "Usage: /task-budget <tokens|none> <seconds|none> [run-id]".to_string(),
                    ));
                };
                let budgets = parse_channel_budget(token_value, "token").and_then(|tokens| {
                    parse_channel_budget(time_value, "time").map(|time| (tokens, time))
                });
                let result = match budgets {
                    Ok((tokens, time)) => {
                        let owned_run_id = run_id.clone();
                        Self::task_runtime_io(
                            store,
                            "update channel TaskRun budgets",
                            move |store| {
                                store.update_run_continuation_budgets(&owned_run_id, tokens, time)
                            },
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
                match result {
                    Ok(_) => format!("TaskRun {run_id} budgets updated."),
                    Err(error) => format!("Unable to update TaskRun {run_id} budgets: {error}"),
                }
            }
            _ => "Unsupported TaskRun command.".to_string(),
        };
        Some(ChannelTaskRunControl::Reply(reply))
    }

    async fn control_command_response(
        &self,
        message: &str,
        conv: &str,
        agent_conv: &str,
        cache_id: &str,
        active_surface_id: &ChannelSurfaceIdentity,
    ) -> Option<String> {
        let mut parts = message.trim().splitn(2, char::is_whitespace);
        let command = parts.next()?;
        let argument = parts.next().map(str::trim).unwrap_or_default();
        match command {
            "/tools" => {
                let runtime = match self.app_state.current_control_runtime().await {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        return Some(format!("Workspace runtime is unavailable: {error}"));
                    }
                };
                Some(
                    echo_agent_app_core::tool_control::execute_tool_control_command(
                        &self.app_state,
                        &runtime.primary_agent(),
                        argument,
                    )
                    .await,
                )
            }
            "/workflow" => Some(
                self.app_state
                    .history
                    .workflows
                    .execute_command(argument)
                    .await
                    .unwrap_or_else(|error| format!("Workflow command failed: {error}")),
            ),
            "/compact" | "/compress" => {
                let keep_messages = if command == "/compress" { 6 } else { 12 };
                let focus = (!argument.is_empty()).then(|| argument.to_string());
                let runtime = match self.app_state.current_control_runtime().await {
                    Ok(runtime) => runtime,
                    Err(error) => {
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
                    Err(error) => return Some(format!("Unable to admit compression: {error}")),
                };
                if let Err(message) = self.publish_active_turn(
                    active_surface_id,
                    conv,
                    agent_conv,
                    &runtime,
                    &turn_id,
                ) {
                    return Some(
                        match settle_channel_turn_after_input_observers(
                            lease,
                            echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                        )
                        .await
                        {
                            Ok(_) => message,
                            Err(error) => {
                                format!("{message}; foreground settlement failed: {error}")
                            }
                        },
                    );
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
                            echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                                echo_agent::error::AgentFailure::message(
                                    "workspace_generation",
                                    message.clone(),
                                ),
                            ),
                        )
                        .await;
                        drop(active_owner);
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
                            echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                                echo_agent::error::AgentFailure::message(
                                    "agent_pool",
                                    error.to_string(),
                                ),
                            ),
                        )
                        .await;
                        drop(active_owner);
                        return Some(match settlement {
                            Ok(_) => message,
                            Err(error) => {
                                format!("{message}; foreground settlement failed: {error}")
                            }
                        });
                    }
                };
                let agent = execution.agent();
                configure_channel_agent(&agent, cache_id, Arc::clone(&self.hitl)).await;
                let workspace_io_receipt = runtime.workspace_io_receipt();
                let app_state = Arc::clone(&self.app_state);
                let workspace_id = runtime.execution_scope().workspace_id().to_string();
                let conversation_id = conv.to_string();
                let agent_conversation_id = agent_conv.to_string();
                let compression_turn_id = turn_id.clone();
                let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                if let Err(error) =
                    self.foreground_turns
                        .supervise(lease, move |lease| async move {
                            let compression = app_state
                                .compress_conversation_with_agent(
                                    &workspace_id,
                                    &conversation_id,
                                    &agent_conversation_id,
                                    &compression_turn_id,
                                    &agent,
                                    focus,
                                    keep_messages,
                                    workspace_io_receipt,
                                    Some(lease.cancellation_token()),
                                )
                                .await;
                            drop(execution);
                            receipts.release_lifo();
                            let (outcome, message) = match compression {
                                Err(
                                    echo_agent_app_core::manual_compression::ManualCompressionError::Cancelled,
                                ) => (
                                    echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                                    "Context compression was cancelled.".to_string(),
                                ),
                                Ok(receipt) => (
                                    echo_agent_app_core::chat_driver::TurnOutcome::Completed,
                                    format!(
                                        "Context compressed: {} -> {} messages, {} tokens saved.",
                                        receipt.messages_before,
                                        receipt.messages_after,
                                        receipt.tokens_saved()
                                    ),
                                ),
                                Err(error) => (
                                    echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                                            echo_agent::error::AgentFailure::message(
                                                error.code(),
                                                error.to_string(),
                                            ),
                                        ),
                                    format!("Context compression failed: {error}"),
                                ),
                            };
                            let message = match settle_channel_turn_after_input_observers(
                                lease, outcome,
                            )
                            .await
                            {
                                Ok(_) => message,
                                Err(error) => {
                                    format!("{message}; foreground settlement failed: {error}")
                                }
                            };
                            drop(active_owner);
                            let _delivered = result_tx.send(message);
                        })
                {
                    Self::clear_active_turn(&self.active_turns, active_surface_id, &turn_id);
                    return Some(format!("Unable to supervise compression: {error}"));
                }
                Some(result_rx.await.unwrap_or_else(|_| {
                    "Compression owner ended without publishing its terminal result.".to_string()
                }))
            }
            "/stop" => {
                let active = match self
                    .resolve_active_turn(active_surface_id, conv, agent_conv)
                    .await
                {
                    Ok(active) => active,
                    Err(error) => {
                        return Some(format!("Unable to inspect the active turn: {error}"));
                    }
                };
                let Some(active) = active else {
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
                    Self::clear_active_turn(&self.active_turns, active_surface_id, &active.turn_id);
                }
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
            "/cancel" => Some(match self.hitl.reject_front("Cancelled by user").await {
                ChannelHumanLoopResolution::NoPending => {
                    "No pending approval or input request to cancel.".to_string()
                }
                ChannelHumanLoopResolution::Resolved(message)
                | ChannelHumanLoopResolution::Rejected(message) => message,
            }),
            "/reset" => {
                let active = match self
                    .resolve_active_turn(active_surface_id, conv, agent_conv)
                    .await
                {
                    Ok(active) => active,
                    Err(error) => {
                        return Some(format!("Unable to inspect the active turn: {error}"));
                    }
                };
                let runtime = match active.as_ref() {
                    Some(active) => active.runtime.clone(),
                    None => match self.app_state.current_control_runtime().await {
                        Ok(runtime) => runtime,
                        Err(error) => {
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
                        return Some(format!(
                            "Unable to reset before the active turn settles: {error}"
                        ));
                    }
                    if !barrier_complete {
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
                    Err(echo_agent_app_core::conversation_deletion::ConversationDeletionError::Foreground(
                        ForegroundTurnError::Busy { active_turn_id, .. },
                    )) => {
                        return Some(format!(
                            "Turn {active_turn_id} started before reset admission; stop it and retry."
                        ));
                    }
                    Err(error) => return Some(format!("Unable to admit reset: {error}")),
                };
                if let Err(message) = self.publish_active_turn(
                    active_surface_id,
                    conv,
                    agent_conv,
                    &runtime,
                    &reset_turn_id,
                ) {
                    return Some(
                        match settle_channel_turn_after_input_observers(
                            reset_lease,
                            echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                        )
                        .await
                        {
                            Ok(_) => message,
                            Err(error) => {
                                format!("{message}; foreground settlement failed: {error}")
                            }
                        },
                    );
                }
                let generation_receipts = match Self::generation_receipts(&runtime) {
                    Ok(receipts) => receipts,
                    Err(message) => {
                        let settlement = settle_channel_turn_after_input_observers(
                            reset_lease,
                            echo_agent_app_core::chat_driver::TurnOutcome::Failed(
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
                if let Err(error) =
                    self.foreground_turns
                        .supervise(reset_lease, move |reset_lease| async move {
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
                                                echo_agent_app_core::chat_driver::TurnOutcome::Completed,
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
                                                echo_agent_app_core::chat_driver::TurnOutcome::Failed(
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
                            let message = match settle_channel_turn_after_input_observers(
                                reset_lease,
                                outcome,
                            )
                            .await
                            {
                                Ok(_) => message,
                                Err(error) => {
                                    format!("{message}; foreground settlement failed: {error}")
                                }
                            };
                            Self::clear_active_turn(
                                &active_turns,
                                &active_surface_id_for_owner,
                                &reset_turn_id_for_owner,
                            );
                            let _delivered = result_tx.send(message);
                        })
                {
                    Self::clear_active_turn(&self.active_turns, active_surface_id, &reset_turn_id);
                    return Some(format!("Unable to supervise reset settlement: {error}"));
                }
                Some(result_rx.await.unwrap_or_else(|_| {
                    "Reset owner ended without publishing its terminal result.".to_string()
                }))
            }
            _ => None,
        }
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
    snapshot: &echo_agent_app_core::tasks::task_runtime::RunStateSnapshot,
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
    report: &echo_agent_app_core::tasks::task_runtime::CompletionGateReport,
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
    Request(echo_agent_app_core::extension_commands::ExtensionCommandRequest),
    ParseFailure {
        kind: echo_agent_app_core::extension_commands::ExtensionKind,
        identity: echo_agent_app_core::extension_commands::ExtensionCommandIdentity,
        error: String,
    },
}

#[cfg(feature = "channels")]
fn channel_extension_scope_from_product_data(
    workspace_id: &str,
    workspace_generation: String,
    sender_id: &str,
    sender_incarnation: &str,
) -> Result<echo_agent_app_core::extension_commands::ExtensionRequestScope, String> {
    echo_agent_app_core::extension_commands::ExtensionRequestScope::new(
        workspace_id,
        workspace_generation,
        Some(sender_id.to_string()),
        Some(sender_incarnation.to_string()),
    )
    .map_err(|error| error.to_string())
}

#[cfg(feature = "channels")]
async fn channel_extension_scope_for_runtime(
    state: &echo_agent_app_core::state::AppState,
    runtime: &echo_agent_app_core::state::ScopedChatRuntime,
    sender_id: &str,
    sender_incarnation: &str,
) -> Result<echo_agent_app_core::extension_commands::ExtensionRequestScope, String> {
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
    let identity = echo_agent_app_core::extension_commands::ExtensionCommandIdentity {
        request_id: request_id.to_string(),
        operation_id: operation_id.to_string(),
    };
    match echo_agent_app_core::extension_commands::parse_extension_command(
        message,
        identity.clone(),
    ) {
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
impl echo_agent_app_core::chat_driver::ChatSink for ChannelManagementJournalSink {
    fn on_event(&self, _event: echo_agent_app_core::chat_driver::ChatDriverEvent) -> bool {
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
    if !echo_agent_app_core::developer_commands::DeveloperCommandRegistry::commands()
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
fn render_channel_reflection_receipt(
    receipt: &echo_agent_app_core::reflection::ReflectionReceipt,
) -> String {
    receipt.display_message()
}

#[cfg(all(feature = "channels", test))]
mod reflection_adapter_tests {
    #[test]
    fn channel_parser_and_projection_use_the_shared_contract() -> Result<(), String> {
        let parsed = echo_agent_app_core::reflection::ReflectionCommand::parse("/reflect")
            .map_err(|error| error.to_string())?;
        assert!(parsed.is_some());
        let receipt = echo_agent_app_core::reflection::reflection_receipt_fixture();
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

    async fn handle_stream<'a>(
        &'a self,
        mut msg: echo_agent::channels::InboundMessage,
    ) -> echo_agent::error::Result<
        futures::stream::BoxStream<
            'a,
            echo_agent::error::Result<echo_agent::channels::OutboundMessage>,
        >,
    > {
        if let Some(error) = self.initialization_error.as_ref() {
            return Ok(immediate_channel_response(
                &msg,
                format!("Channel session initialization failed: {error}"),
            ));
        }
        if let Some(error) = self
            .incarnation_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return Ok(immediate_channel_response(
                &msg,
                format!("Channel session is quarantined: {error}"),
            ));
        }
        if self.session_instance.channel_id() != msg.channel_id
            || self.session_instance.conversation_id() != msg.conversation_id()
            || self.session_instance.sender_id() != msg.sender_id
        {
            return Ok(immediate_channel_response(
                &msg,
                "Channel session identity changed after framework routing.",
            ));
        }
        let conv = Self::conversation_id(&msg.channel_id, msg.conversation_id(), &msg.sender_id);
        let incarnation_id = self.session_instance.incarnation_id();
        let agent_conv = Self::agent_conversation_id(&conv, &incarnation_id);
        let active_surface_id =
            Self::active_surface_identity(&msg.channel_id, msg.conversation_id(), &msg.sender_id);
        let cache_id = Self::cache_user_id(&conv, &incarnation_id);
        let extension_operation_id =
            format!("channel-extension:{}:{}", msg.channel_id, msg.message_id);
        let extension_input = match parse_channel_extension_input(
            &msg.text,
            &msg.message_id,
            &extension_operation_id,
        ) {
            Ok(input) => input,
            Err(error) => return Ok(immediate_channel_response(&msg, error)),
        };
        if let Some(extension_input) = extension_input {
            let retirement_runtime = match self.app_state.current_control_runtime().await {
                Ok(runtime) => runtime,
                Err(error) => {
                    return Ok(immediate_channel_response(
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
                return Ok(immediate_channel_response(&msg, error));
            }
            let turn_id = format!("channel-extension-turn:{}", msg.message_id);
            let (runtime, lease) = match self
                .app_state
                .begin_scoped_chat_turn_owned(
                    ForegroundTurnSurface::Channel,
                    &conv,
                    turn_id.clone(),
                )
                .await
            {
                Ok(lease) => lease,
                Err(echo_agent_app_core::state::ScopedChatTurnError::Conversation(
                    echo_agent_app_core::conversation_deletion::ConversationDeletionError::Foreground(
                        ForegroundTurnError::Busy { active_turn_id, .. },
                    ),
                )) => {
                    return Ok(immediate_channel_response(
                        &msg,
                        format!("Turn {active_turn_id} is still running; command was not applied."),
                    ));
                }
                Err(error) => {
                    return Ok(immediate_channel_response(
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
                    echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                )
                .await
                {
                    Ok(_) => message,
                    Err(error) => format!("{message}; foreground settlement failed: {error}"),
                };
                return Ok(immediate_channel_response(&msg, message));
            }
            let _active_owner = ChannelActiveTurnOwner::new(
                Arc::clone(&self.active_turns),
                active_surface_id.clone(),
                turn_id.clone(),
            );
            self.record_runtime_owner(&runtime, &conv, &agent_conv);
            let scope = runtime.execution_scope().workspace_id().to_string();
            let receipt = match extension_input {
                ChannelExtensionInput::Request(mut request) if msg.attachments.is_empty() => {
                    match channel_extension_scope_for_runtime(
                        self.app_state.as_ref(),
                        &runtime,
                        &msg.sender_id,
                        &incarnation_id,
                    )
                    .await
                    {
                        Ok(request_scope) => {
                            request.scope = Some(request_scope);
                            echo_agent_app_core::extension_commands::ExtensionCommandDispatcher::new(
                                self.app_state.clone(),
                            )
                            .dispatch(request, Some(runtime.clone()), conv.clone())
                            .await
                        }
                        Err(error) => {
                            echo_agent_app_core::extension_commands::ExtensionCommandReceipt::failed(
                                request.kind(),
                                request.identity(),
                                scope.clone(),
                                error.to_string(),
                            )
                        }
                    }
                }
                ChannelExtensionInput::Request(request) => {
                    echo_agent_app_core::extension_commands::ExtensionCommandReceipt::failed(
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
                } => echo_agent_app_core::extension_commands::ExtensionCommandReceipt::failed(
                    kind,
                    identity,
                    scope.clone(),
                    error,
                ),
            };
            let message = receipt.display_message();
            let outcome = crate::cli::modes::extension_receipt_terminal(&receipt);
            let sink = echo_agent_app_core::chat_event_log::bind_surface_chat_sink(
                echo_agent_app_core::chat_event_log::ChatSurface::Channel,
                Arc::new(ChannelManagementJournalSink),
                self.app_state.storage.chat_events.clone(),
                self.app_state.storage.tool_executions.clone(),
                scope,
                Some(conv.clone()),
                turn_id,
            );
            if !sink.on_event(
                echo_agent_app_core::chat_driver::ChatDriverEvent::ExtensionReceipt(Box::new(
                    receipt,
                )),
            ) {
                let failure = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "channel_journal",
                        "Channel journal rejected the Extension receipt",
                    ),
                );
                let settlement = settle_channel_turn_after_input_observers(lease, failure).await;
                let message = settlement.err().map_or_else(
                    || "Channel journal rejected the Extension receipt.".to_string(),
                    |error| format!("Channel journal rejected the Extension receipt; foreground settlement failed: {error}"),
                );
                return Ok(immediate_channel_response(&msg, message));
            }
            let terminal_delivered = sink.on_event(
                echo_agent_app_core::chat_driver::ChatDriverEvent::TurnStatus {
                    status: outcome.status().to_string(),
                },
            );
            if !terminal_delivered {
                let failure = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "channel_journal",
                        "Channel journal rejected the Extension terminal",
                    ),
                );
                let settlement = settle_channel_turn_after_input_observers(lease, failure).await;
                let message = settlement.err().map_or_else(
                    || "Channel journal rejected the Extension terminal.".to_string(),
                    |error| format!("Channel journal rejected the Extension terminal; foreground settlement failed: {error}"),
                );
                return Ok(immediate_channel_response(&msg, message));
            }
            let message = match settle_channel_turn_after_input_observers(lease, outcome).await {
                Ok(_) => message,
                Err(error) => format!("{message}; foreground settlement failed: {error}"),
            };
            return Ok(immediate_channel_response(&msg, message));
        }
        let reflection_command =
            match echo_agent_app_core::reflection::ReflectionCommand::parse(&msg.text) {
                Ok(command) => command,
                Err(error) => return Ok(immediate_channel_response(&msg, error.to_string())),
            };
        if reflection_command.is_some() {
            if !msg.attachments.is_empty() {
                return Ok(immediate_channel_response(
                    &msg,
                    "/reflect does not accept attachments",
                ));
            }
            let runtime = match self.app_state.current_control_runtime().await {
                Ok(runtime) => runtime,
                Err(error) => {
                    return Ok(immediate_channel_response(
                        &msg,
                        format!("Reflection unavailable: {error}"),
                    ));
                }
            };
            let execution = match runtime.agent_for(&agent_conv).await {
                Ok(execution) => execution,
                Err(error) => {
                    return Ok(immediate_channel_response(
                        &msg,
                        format!("Reflection unavailable: {error}"),
                    ));
                }
            };
            let agent = execution.agent();
            let message = match echo_agent_app_core::reflection::reflect_session(
                &runtime,
                &agent,
                Some(&conv),
            )
            .await
            {
                Ok(receipt) => render_channel_reflection_receipt(&receipt),
                Err(error) => format!("Reflection failed: {error}"),
            };
            return Ok(immediate_channel_response(&msg, message));
        }
        let developer_command = match parse_developer_command(&msg.text) {
            Ok(command) => command,
            Err(error) => return Ok(immediate_channel_response(&msg, error)),
        };
        if let Some((command, args)) = developer_command {
            // Subscribe before dispatch so a fast shell cannot exit before
            // this channel starts observing its output.
            let terminal_events = self.app_state.terminal.subscribe();
            let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            let browser_conversation =
                Self::conversation_id(&msg.channel_id, msg.conversation_id(), &msg.sender_id);
            let registry = echo_agent_app_core::developer_commands::DeveloperCommandRegistry::new(
                self.app_state.terminal.clone(),
                Some(self.app_state.clone()),
            )
            .with_browser_conversation_id(browser_conversation);
            return match registry.execute(&command, &arg_refs).await {
                Ok(output) => match output.attached_terminal {
                    Some(terminal_id) => Ok(channel_terminal_stream(
                        &msg,
                        output.message,
                        terminal_events,
                        terminal_id,
                    )),
                    None => Ok(immediate_channel_response(&msg, output.message)),
                },
                Err(error) => Ok(immediate_channel_response(
                    &msg,
                    format!("/{command} failed: {error}"),
                )),
            };
        }
        if let Some(message) = self.agent_router_command_response(&msg.text, &conv).await {
            return Ok(immediate_channel_response(&msg, message));
        }
        let mut resume_task_run = None;
        if let Some(control) = self.task_run_command_response(&msg.text, &conv).await {
            match control {
                ChannelTaskRunControl::Reply(message) => {
                    return Ok(immediate_channel_response(&msg, message));
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
                return Ok(immediate_channel_response(
                    &msg,
                    "Usage: /steer <additional instruction>",
                ));
            }
            msg.text = instruction.to_string();
        }
        // Product control commands always outrank HITL parsing. `/stop` owns
        // turn cancellation; `/cancel` rejects only the queue front.
        if !explicit_steer
            && let Some(message) = self
                .control_command_response(
                    &msg.text,
                    &conv,
                    &agent_conv,
                    &cache_id,
                    &active_surface_id,
                )
                .await
        {
            return Ok(immediate_channel_response(&msg, message));
        }
        if msg.text.split_whitespace().next() == Some("/extract") {
            use echo_agent_app_core::structured_extraction::PreparedStructuredExtractionCommand;

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
                    return Ok(immediate_channel_response(
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
                    return Ok(immediate_channel_response(
                        &msg,
                        value.unwrap_or_else(|error| {
                            format!("Structured extraction serialization failed: {error}")
                        }),
                    ));
                }
                PreparedStructuredExtractionCommand::Validate(schema) => {
                    let value = serde_json::to_value(
                        self.app_state
                            .history
                            .structured_extraction
                            .validate_schema(&schema),
                    )
                    .and_then(|value| serde_json::to_string_pretty(&value));
                    return Ok(immediate_channel_response(
                        &msg,
                        value.unwrap_or_else(|error| {
                            format!("Structured extraction serialization failed: {error}")
                        }),
                    ));
                }
                PreparedStructuredExtractionCommand::Extract(request) => {
                    let runtime = match self.app_state.current_control_runtime().await {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            return Ok(immediate_channel_response(
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
                        return Ok(immediate_channel_response(&msg, error));
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
                            return Ok(immediate_channel_response(
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
                            echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                        )
                        .await
                        {
                            Ok(_) => message,
                            Err(error) => {
                                format!("{message}; foreground settlement failed: {error}")
                            }
                        };
                        return Ok(immediate_channel_response(&msg, message));
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
                                echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message(
                                        "workspace_generation",
                                        message.clone(),
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
                            return Ok(immediate_channel_response(&msg, message));
                        }
                    };
                    self.record_runtime_owner(&runtime, &conv, &agent_conv);
                    let execution = match runtime.agent_for(&agent_conv).await {
                        Ok(execution) => execution,
                        Err(error) => {
                            receipts.release_lifo();
                            let message = format!("AgentPool admission failed: {error}");
                            let settlement = settle_channel_turn_after_input_observers(
                                lease,
                                echo_agent_app_core::chat_driver::TurnOutcome::Failed(
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
                            return Ok(immediate_channel_response(&msg, message));
                        }
                    };
                    let agent = execution.agent();
                    configure_channel_agent(&agent, &cache_id, Arc::clone(&self.hitl)).await;
                    let cancel = lease.cancellation_token();
                    let extraction = await_channel_operation(
                        cancel,
                        self.app_state
                            .history
                            .structured_extraction
                            .extract(&agent, request),
                    )
                    .await;
                    drop(execution);
                    receipts.release_lifo();
                    let (outcome, message) = match extraction {
                        None => (
                            echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                            "Structured extraction was cancelled.".to_string(),
                        ),
                        Some(Ok(extracted)) => (
                            echo_agent_app_core::chat_driver::TurnOutcome::Completed,
                            serde_json::to_string_pretty(&extracted).unwrap_or_else(|error| {
                                format!("Structured extraction serialization failed: {error}")
                            }),
                        ),
                        Some(Err(error)) => (
                            echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                                echo_agent::error::AgentFailure::message(
                                    error.code(),
                                    error.to_string(),
                                ),
                            ),
                            format!("Structured extraction failed: {error}"),
                        ),
                    };
                    let message =
                        match settle_channel_turn_after_input_observers(lease, outcome).await {
                            Ok(_) => message,
                            Err(error) => {
                                format!("{message}; foreground settlement failed: {error}")
                            }
                        };
                    drop(active_owner);
                    return Ok(immediate_channel_response(&msg, message));
                }
            }
        }
        if !explicit_steer {
            match self.hitl.try_resolve_message(&msg.text) {
                ChannelHumanLoopResolution::Resolved(message)
                | ChannelHumanLoopResolution::Rejected(message) => {
                    return Ok(immediate_channel_response(&msg, message));
                }
                ChannelHumanLoopResolution::NoPending => {}
            }
        }
        // Management commands use the same exact foreground admission and
        // TaskRuntime -> pool order as chat. They do not mutate the agent when
        // any admission step fails.
        if is_agent_management_command(&msg.text) {
            let retirement_runtime = match self.app_state.current_control_runtime().await {
                Ok(runtime) => runtime,
                Err(error) => {
                    return Ok(immediate_channel_response(
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
                return Ok(immediate_channel_response(&msg, error));
            }
            let command_id = uuid::Uuid::new_v4().to_string();
            let (runtime, lease) = match self
                .app_state
                .begin_scoped_chat_turn_owned(
                    ForegroundTurnSurface::Channel,
                    &conv,
                    command_id.clone(),
                )
                .await
            {
                Ok(lease) => lease,
                Err(echo_agent_app_core::state::ScopedChatTurnError::Conversation(
                    echo_agent_app_core::conversation_deletion::ConversationDeletionError::Foreground(
                        ForegroundTurnError::Busy { active_turn_id, .. },
                    ),
                )) => {
                    return Ok(immediate_channel_response(
                        &msg,
                        format!("Turn {active_turn_id} is still running; command was not applied."),
                    ));
                }
                Err(error) => {
                    return Ok(immediate_channel_response(
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
                let message = match settle_channel_turn_after_input_observers(
                    lease,
                    echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                )
                .await
                {
                    Ok(_) => message,
                    Err(error) => format!("{message}; foreground settlement failed: {error}"),
                };
                return Ok(immediate_channel_response(&msg, message));
            }
            let _active_owner = ChannelActiveTurnOwner::new(
                Arc::clone(&self.active_turns),
                active_surface_id.clone(),
                command_id,
            );
            let generation_receipts = match Self::generation_receipts(&runtime) {
                Ok(receipts) => receipts,
                Err(message) => {
                    let settlement = settle_channel_turn_after_input_observers(
                        lease,
                        echo_agent_app_core::chat_driver::TurnOutcome::Failed(
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
                    return Ok(immediate_channel_response(&msg, message));
                }
            };
            self.record_runtime_owner(&runtime, &conv, &agent_conv);
            let pool_execution = match runtime.agent_for(&agent_conv).await {
                Ok(execution) => execution,
                Err(error) => {
                    generation_receipts.release_lifo();
                    let message = format!("AgentPool admission failed: {error}");
                    let settlement = settle_channel_turn_after_input_observers(
                        lease,
                        echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                            echo_agent::error::AgentFailure::message("agent_pool", message.clone()),
                        ),
                    )
                    .await;
                    let message = match settlement {
                        Ok(_) => message,
                        Err(error) => {
                            format!("{message}; foreground settlement failed: {error}")
                        }
                    };
                    return Ok(immediate_channel_response(&msg, message));
                }
            };
            let agent = pool_execution.agent();
            configure_channel_agent(&agent, &cache_id, Arc::clone(&self.hitl)).await;
            let product_data = self.app_state.product_data_for_runtime(&runtime).await.ok();
            let response = if let Some(message) = channel_trace_response(&agent, &msg.text).await {
                ChannelManagementResponse::completed(message)
            } else if let Some(message) =
                channel_analysis_response(product_data.as_ref(), &msg.text).await
            {
                ChannelManagementResponse::completed(message)
            } else if let Some(message) =
                channel_papers_response(product_data.as_ref(), &msg.text).await
            {
                ChannelManagementResponse::completed(message)
            } else {
                ChannelManagementResponse::failed(
                    "management_command",
                    "Unsupported channel management command.",
                )
            };
            drop(pool_execution);
            generation_receipts.release_lifo();
            let message =
                match settle_channel_turn_after_input_observers(lease, response.terminal).await {
                    Ok(_) => response.message,
                    Err(error) => format!(
                        "{}; foreground settlement failed: {error}",
                        response.message
                    ),
                };
            return Ok(immediate_channel_response(&msg, message));
        }

        if channel_resume_rejects_attachments(resume_task_run.is_some(), msg.attachments.len()) {
            return Ok(immediate_channel_response(
                &msg,
                "TaskRun resume does not accept new attachments; send them in a separate turn.",
            ));
        }
        let retirement_runtime = match resume_task_run.as_ref() {
            Some((_, _, runtime)) => runtime.as_ref().clone(),
            None => match self.app_state.current_control_runtime().await {
                Ok(runtime) => runtime,
                Err(error) => {
                    return Ok(immediate_channel_response(
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
            return Ok(immediate_channel_response(&msg, error));
        }
        if resume_task_run.is_none() {
            let address =
                channel_input_address(retirement_runtime.execution_scope().workspace_id(), &conv);
            let input_id = match echo_agent_app_core::conversation_input::stable_scoped_input_id(
                &address,
                echo_agent_app_core::conversation_input::ConversationInputSource::Channel,
                &msg.message_id,
            ) {
                Ok(input_id) => input_id,
                Err(error) => {
                    return Ok(immediate_channel_response(
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
                    return Ok(immediate_channel_response(
                        &msg,
                        format!("Unable to persist channel input: {error}"),
                    ));
                }
            };
            if !submitted.is_dispatchable() {
                return Ok(immediate_channel_response(
                    &msg,
                    format!(
                        "Channel input {} is already {}.",
                        submitted.identity.input_id,
                        channel_input_phase_label(submitted.phase)
                    ),
                ));
            }
            let (reply_route, reply_receiver) = input_pump::channel_input_reply_route(&msg);
            if let Err(error) = self
                .settle_pending_retirement_for_runtime(
                    &retirement_runtime,
                    &conv,
                    &agent_conv,
                    &active_surface_id,
                )
                .await
            {
                return Ok(immediate_channel_response(
                    &msg,
                    format!("Channel input remains persisted: {error}"),
                ));
            }
            if self.active_turn(&active_surface_id).is_some() {
                match self
                    .route_live_conversation_input(
                        &active_surface_id,
                        &conv,
                        &agent_conv,
                        submitted.clone(),
                        reply_route,
                    )
                    .await
                {
                    Ok(ChannelLiveInputRoute::Routed) => {
                        return Ok(channel_input_response_stream(
                            reply_receiver,
                            self.app_state.storage.tool_executions.clone(),
                        )
                        .await);
                    }
                    Ok(ChannelLiveInputRoute::PumpPending(reply_route)) => {
                        if let Err(error) = self
                            .input_pump
                            .register_reply(submitted.identity.clone(), reply_route)
                        {
                            return Ok(immediate_channel_response(
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
                            return Ok(immediate_channel_response(
                                &msg,
                                format!("Channel input is durable but its pump failed: {error}"),
                            ));
                        }
                        return Ok(channel_input_response_stream(
                            reply_receiver,
                            self.app_state.storage.tool_executions.clone(),
                        )
                        .await);
                    }
                    Err(error) => {
                        return Ok(immediate_channel_response(
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
                    return Ok(immediate_channel_response(
                        &msg,
                        format!("Channel input is durable but its response route failed: {error}"),
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
                    return Ok(immediate_channel_response(
                        &msg,
                        format!("Channel input is durable but its pump failed: {error}"),
                    ));
                }
                return Ok(channel_input_response_stream(
                    reply_receiver,
                    self.app_state.storage.tool_executions.clone(),
                )
                .await);
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
                .map_err(echo_agent_app_core::state::ScopedChatTurnError::from),
            None => {
                self.app_state
                    .begin_scoped_chat_turn_owned(
                        ForegroundTurnSurface::Channel,
                        &conv,
                        turn_id.clone(),
                    )
                    .await
            }
        };
        let (scoped_runtime, foreground_lease) = match admission {
            Ok(admission) => admission,
            Err(echo_agent_app_core::state::ScopedChatTurnError::Conversation(
                echo_agent_app_core::conversation_deletion::ConversationDeletionError::Foreground(
                    ForegroundTurnError::Busy { active_turn_id, .. },
                ),
            )) => {
                return Ok(immediate_channel_response(
                    &msg,
                    format!("Turn {active_turn_id} won admission; channel input remains queued."),
                ));
            }
            Err(error) => {
                return Ok(immediate_channel_response(
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
                echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
            )
            .await
            {
                Ok(_) => message,
                Err(error) => format!("{message}; foreground settlement failed: {error}"),
            };
            return Ok(immediate_channel_response(&msg, message));
        }
        self.record_runtime_owner(&scoped_runtime, &conv, &agent_conv);
        let Some(pool) = scoped_runtime.pool() else {
            let failure = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(
                    "agent_pool",
                    "The active workspace has no AgentPool",
                ),
            );
            let settlement =
                settle_channel_turn_after_input_observers(foreground_lease, failure).await;
            Self::clear_active_turn(&self.active_turns, &active_surface_id, &turn_id);
            if let Err(error) = settlement {
                return Ok(immediate_channel_response(
                    &msg,
                    format!("AgentPool unavailable and foreground settlement failed: {error}"),
                ));
            }
            return Ok(immediate_channel_response(
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
                            echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(
                                format!("TaskRun '{run_id}' no longer exists"),
                            )
                        })?;
                        expected_identity.validate_resumable(&snapshot).map_err(|error| {
                            echo_agent_app_core::tasks::task_runtime::StoreError::InvalidPlan(error)
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
                    echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message("task_run_resume", detail.clone()),
                    ),
                )
                .await;
                Self::clear_active_turn(&self.active_turns, &active_surface_id, &turn_id);
                if let Err(error) = settlement {
                    return Ok(immediate_channel_response(
                        &msg,
                        format!("{detail}; foreground settlement failed: {error}"),
                    ));
                }
                return Ok(immediate_channel_response(&msg, detail));
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
                let failure = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "product_data_admission",
                        detail.clone(),
                    ),
                );
                let settlement =
                    settle_channel_turn_after_input_observers(foreground_lease, failure).await;
                Self::clear_active_turn(&self.active_turns, &active_surface_id, &turn_id);
                if let Err(settlement_error) = settlement {
                    return Ok(immediate_channel_response(
                        &msg,
                        format!("{detail}; foreground settlement failed: {settlement_error}"),
                    ));
                }
                return Ok(immediate_channel_response(&msg, detail));
            }
        };
        let webhook_emitter = self.webhook_emitter.clone();
        let review_integration = scoped_runtime.review_integration();
        let hitl = Arc::clone(&self.hitl);
        let prompt_rx = self.hitl.subscribe_prompts();
        let conv_owned = conv.clone();
        let agent_conv_owned = agent_conv.clone();
        let resume_dispatch = resume_task_run.map(|(expected_resume, continuation_enabled, _)| {
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
        let supervision =
            self.foreground_turns
                .supervise(foreground_lease, move |foreground_lease| async move {
                    let _active_owner = active_owner;
                    use echo_agent_app_core::foreground_turn::drive_foreground_pooled_chat_turn;

                    let renderer: std::sync::Arc<dyn echo_agent_app_core::chat_driver::ChatSink> =
                        std::sync::Arc::new(ChannelSurfaceSink::new(tx, renderer_cancel));
                    let sink = echo_agent_app_core::chat_event_log::bind_surface_chat_sink(
                        echo_agent_app_core::chat_event_log::ChatSurface::Channel,
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
                            let outcome = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
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
                            let _delivered = terminal_tx.send(outcome);
                            return;
                        }
                    };
                    let res =
                        std::sync::Arc::new(echo_agent_app_core::chat_resources::ChatResources {
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
                                let outcome = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
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
                                let _delivered = terminal_tx.send(outcome);
                                return;
                            }
                        };
                        let agent = execution.agent();
                        if let Err(error) = configure(agent.clone()).await {
                            let outcome = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
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
                                echo_agent_app_core::chat_driver::subagent_trace_sink_for(
                                    &res.sink,
                                );
                            let launch = match res.store.clone() {
                            Some(store) => {
                                echo_agent_app_core::tasks::task_runtime::launch_planned_run_resume(
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
                                .map_err(|error| error.to_string())
                            }
                            None => Err("TaskRuntime store is unavailable".to_string()),
                        };
                            let outcome = match launch {
                            Ok(launch) => match launch.wait().await {
                                Ok(
                                    echo_agent_app_core::tasks::task_runtime::RunOutcome::Completed,
                                ) => echo_agent_app_core::chat_driver::TurnOutcome::Completed,
                                Ok(
                                    echo_agent_app_core::tasks::task_runtime::RunOutcome::Cancelled,
                                ) => echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                                Ok(other) => echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message(
                                        "planned_resume",
                                        format!("planned resume ended with {other:?}"),
                                    ),
                                ),
                                Err(error) => {
                                    echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                                        echo_agent::error::AgentFailure::message(
                                            "planned_resume",
                                            error,
                                        ),
                                    )
                                }
                            },
                            Err(error) => echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                                echo_agent::error::AgentFailure::message("planned_resume", error),
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
                                drive_foreground_pooled_chat_turn(
                                    foreground_lease,
                                    pool,
                                    agent_conv_owned,
                                    configure,
                                    &turn,
                                    res,
                                    binding,
                                )
                                .await
                            }
                            None => {
                                let outcome = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
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
                    let _delivered = terminal_tx.send(outcome);
                });
        if let Err(error) = supervision {
            Self::clear_active_turn(&self.active_turns, &active_surface_id, &turn_id);
            return Ok(immediate_channel_response(
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
        Ok(aggregate_by_sentence_with_repository(
            event_stream,
            channel_id,
            to,
            chat_type,
            self.app_state.storage.tool_executions.clone(),
        )
        .await)
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
async fn configure_channel_agent(
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    cache_id: &str,
    hitl: Arc<ChannelHumanLoopProvider>,
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
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    message: &str,
) -> Option<String> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/trace" {
        return None;
    }
    let store = agent.read(|agent| agent.run_store.clone()).await;
    let Some(store) = store else {
        return Some("Run diagnostics are not configured.".to_string());
    };
    let diagnostic_id = match parts.next() {
        Some(value) => value.to_string(),
        None => {
            match echo_agent_app_core::observability::list_diagnostic_runs(store.as_ref()).await {
                Ok(runs) => match runs.first() {
                    Some(run) => run.diagnostic_id.clone(),
                    None => return Some("No durable run diagnostics available.".to_string()),
                },
                Err(error) => return Some(format!("Unable to list run diagnostics: {error}")),
            }
        }
    };
    Some(
        match echo_agent_app_core::observability::load_run_diagnostics(
            store.as_ref(),
            &diagnostic_id,
            None,
        )
        .await
        {
            Ok(Some(diagnostics)) => {
                echo_agent_app_core::observability::format_run_diagnostics(&diagnostics)
            }
            Ok(None) => format!("Run diagnostics not found: {diagnostic_id}"),
            Err(error) => format!("Unable to load run diagnostics: {error}"),
        },
    )
}

#[cfg(feature = "channels")]
async fn channel_analysis_response(
    product_data: Option<&echo_agent_app_core::product_data_io::ScopedProductData>,
    message: &str,
) -> Option<String> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/analysis" {
        return None;
    }
    let args: Vec<&str> = parts.collect();
    Some(match product_data {
        Some(product_data) => {
            crate::cli::cmd_impls::analysis::execute_analysis_command(product_data, &args).await
        }
        None => "Analysis workspace is unavailable.".to_string(),
    })
}

#[cfg(feature = "channels")]
async fn channel_papers_response(
    product_data: Option<&echo_agent_app_core::product_data_io::ScopedProductData>,
    message: &str,
) -> Option<String> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/papers" {
        return None;
    }
    let args: Vec<&str> = parts.collect();
    Some(match product_data {
        Some(product_data) => {
            crate::cli::cmd_impls::research::execute_papers_command(product_data, &args).await
        }
        None => "Research workspace is unavailable.".to_string(),
    })
}

#[cfg(feature = "channels")]
struct ChannelManagementResponse {
    message: String,
    terminal: echo_agent_app_core::chat_driver::TurnOutcome,
}

#[cfg(feature = "channels")]
impl ChannelManagementResponse {
    fn completed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            terminal: echo_agent_app_core::chat_driver::TurnOutcome::Completed,
        }
    }

    fn failed(code: &'static str, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            terminal: echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(code, message.clone()),
            ),
            message,
        }
    }
}

#[cfg(feature = "channels")]
fn channel_attachment_data(
    index: usize,
    attachment: &echo_agent::channels::MessageAttachment,
) -> echo_agent_app_core::types::AttachmentData {
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
    let inferred = echo_agent_app_core::attachments::infer_mime_type(&name);
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
    echo_agent_app_core::types::AttachmentData {
        name,
        mime_type: mime_type.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(&attachment.data),
        size: u64::try_from(attachment.data.len()).unwrap_or(u64::MAX),
        source: echo_agent_app_core::types::AttachmentSource::Channel,
    }
}

#[cfg(all(feature = "channels", test))]
fn stage_channel_attachments(
    attachments: &[echo_agent::channels::MessageAttachment],
    execution_root: &Path,
) -> Result<Vec<echo_agent_app_core::attachments::AttachmentRef>, String> {
    let mut staged = Vec::with_capacity(attachments.len());
    for (index, attachment) in attachments.iter().enumerate() {
        let data = channel_attachment_data(index, attachment);
        match echo_agent_app_core::attachments::stage_attachment_data(&data, Some(execution_root)) {
            Ok(reference) => staged.push(reference),
            Err(error) => {
                let cleanup =
                    echo_agent_app_core::attachments::discard_staged_attachment_refs(&staged).err();
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
    attachments: &[echo_agent_app_core::types::AttachmentData],
    execution_root: &Path,
) -> Result<Vec<echo_agent_app_core::attachments::AttachmentRef>, String> {
    let mut staged = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        match echo_agent_app_core::attachments::stage_attachment_data(
            attachment,
            Some(execution_root),
        ) {
            Ok(reference) => staged.push(reference),
            Err(error) => {
                let cleanup =
                    echo_agent_app_core::attachments::discard_staged_attachment_refs(&staged).err();
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
    attachments: Vec<echo_agent_app_core::types::AttachmentData>,
    execution_root: std::path::PathBuf,
    text: String,
    conversation_id: String,
    turn_id: String,
    runtime_authored: bool,
    workspace_io_receipt: echo_agent_app_core::state::ScopedWorkspaceIoReceipt,
}

#[cfg(feature = "channels")]
async fn prepare_channel_turn(
    preparation: ChannelTurnPreparation,
    flow: &echo_agent_app_core::product_data_io::ProductDataIoFlow,
) -> Result<echo_agent_app_core::prepared_turn::PreparedUserTurn, String> {
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
        let spill_dir =
            echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(&execution_root));
        match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
            echo_agent_app_core::prepared_turn::UserTurnInput {
                text: &text,
                attachments: &attachment_refs,
                spill_dir: &spill_dir,
                conversation_id: Some(&conversation_id),
                turn_id: Some(&turn_id),
            },
        ) {
            Ok(turn) if runtime_authored => Ok(turn.runtime_authored()),
            Ok(turn) => Ok(turn),
            Err(error) => {
                let cleanup = echo_agent_app_core::attachments::discard_staged_attachment_refs(
                    &attachment_refs,
                )
                .err();
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

/// 句末标点(中英文)触发 flush。
#[cfg(feature = "channels")]
fn is_sentence_end(c: char) -> bool {
    // 中文句末:。 ． ！ ？ … ;英文句末:. ! ?
    matches!(c, '。' | '．' | '！' | '？' | '…' | '.' | '!' | '?')
}

/// 把共享 `ChatDriverEvent` 流投影为有界的内部文本 drafts。
///
/// flush 条件(满足任一):
/// 1. buf 含换行 → flush 到最后一个换行(含),保留换行后的剩余。
/// 2. buf 以句末标点结尾 → flush 全 buf。
/// 3. buf.chars().count() >= FLUSH_THRESHOLD → flush 全 buf。
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
        echo_agent_app_core::tool_execution::ToolExecutionRepository::without_initialization(
            std::env::temp_dir().join(format!("eko-channel-render-test-{}", uuid::Uuid::new_v4())),
        ),
    );
    aggregate_by_sentence_with_repository(events, channel_id, to, chat_type, repository).await
}

#[cfg(feature = "channels")]
async fn aggregate_by_sentence_with_repository<'a>(
    mut events: futures::stream::BoxStream<'a, echo_agent::error::Result<ChannelRenderEvent>>,
    channel_id: String,
    to: String,
    chat_type: echo_agent::channels::ChatType,
    tool_executions: Arc<echo_agent_app_core::tool_execution::ToolExecutionRepository>,
) -> futures::stream::BoxStream<'a, echo_agent::error::Result<echo_agent::channels::OutboundMessage>>
{
    use echo_agent::agent::AgentEvent;
    use echo_agent_app_core::chat_driver::ChatDriverEvent;
    use futures::StreamExt;

    let s = async_stream::try_stream! {
        let mut buf = String::new();
        let mut tool_state = ChannelToolRenderState::default();
        let mut tool_capacity_reported = false;
        let mut tool_identity_conflict_reported = false;
        // flush 全 buf(若非空)的统一动作,被多个终态/flush 分支共用。
        macro_rules! flush_all {
            () => {
                if !buf.is_empty() {
                    yield ChannelOutboundDraft::stream(std::mem::take(&mut buf));
                }
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
                ChannelRenderEvent::Token(t) => {
                    buf.push_str(&t);
                    if let Some(trailing_chars) = buf.chars().rev().position(|ch| ch == '\n') {
                        let cut = buf.chars().count().saturating_sub(trailing_chars);
                        let chunk: String = buf.chars().take(cut).collect();
                        buf = buf.chars().skip(cut).collect();
                        yield ChannelOutboundDraft::stream(chunk);
                    } else if buf.chars().last().map(is_sentence_end).unwrap_or(false)
                        || buf.chars().count() >= FLUSH_THRESHOLD
                    {
                        flush_all!();
                    }
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
                AgentEvent::Token(t) => {
                    buf.push_str(&t);
                    // 1. 换行 flush(到最后一个 \n 含)。反向字符偏移表示换行后
                    //    还有多少字符,因此 `cut` 是包含换行的字符数。
                    if let Some(trailing_chars) = buf.chars().rev().position(|ch| ch == '\n') {
                        let cut = buf.chars().count().saturating_sub(trailing_chars);
                        let chunk: String = buf.chars().take(cut).collect();
                        buf = buf.chars().skip(cut).collect();
                        yield ChannelOutboundDraft::stream(chunk);
                    }
                    // 2/3. 句末标点 或 阈值(chars().count() 非字节)→ flush 全 buf
                    else if buf.chars().last().map(is_sentence_end).unwrap_or(false)
                        || buf.chars().count() >= FLUSH_THRESHOLD
                    {
                        flush_all!();
                    }
                }
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
                AgentEvent::FinalAnswer(_) => {
                    flush_all!();
                    tool_state.finish_owner(&chat_owner);
                }
                AgentEvent::Cancelled => {
                    flush_all!();
                    tool_state.finish_owner(&chat_owner);
                }
                AgentEvent::Error { .. } => {
                    flush_all!();
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
                        == echo_agent_app_core::tasks::task_runtime::executor::ExecEventScope::Subagent
                        && let Some(subagent_run_id) = event.subagent_run_id.as_deref()
                    {
                        let owner = ChannelToolOwner::Subagent(subagent_run_id.to_string());
                        match event.event {
                            echo_agent_app_core::tasks::task_runtime::RuntimeEventKind::ToolStarted => {
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
                            echo_agent_app_core::tasks::task_runtime::RuntimeEventKind::ToolOutput => {
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
                            echo_agent_app_core::tasks::task_runtime::RuntimeEventKind::ToolCompleted => {
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
                            echo_agent_app_core::tasks::task_runtime::RuntimeEventKind::Cancelled
                            | echo_agent_app_core::tasks::task_runtime::RuntimeEventKind::TimedOut
                            | echo_agent_app_core::tasks::task_runtime::RuntimeEventKind::Completed
                            | echo_agent_app_core::tasks::task_runtime::RuntimeEventKind::Failed => {
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
                ChannelRenderEvent::Driver(ChatDriverEvent::InputLifecycle(fact)) => {
                    flush_all!();
                    let phase = channel_input_fact_phase(&fact)
                        .map(channel_input_phase_label)
                        .unwrap_or("reordered");
                    yield ChannelOutboundDraft::ordinary(format!(
                        "[input:{}] {}",
                        channel_safe_text(
                            &fact.identity().input_id,
                            CHANNEL_TOOL_PROGRESS_CHARS,
                        ),
                        phase,
                    ));
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::CommandCellStarted { cell }) => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[cell:{}] started: {}", cell.cell_id, cell.name));
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::CommandCellSettled { cell }) => {
                    flush_all!();
                    yield ChannelOutboundDraft::ordinary(format!("[cell:{}] settled: {}", cell.cell_id, cell.phase));
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::AwaiterResultReady { result }) => {
                    flush_all!();
                    let event = ChatDriverEvent::AwaiterResultReady { result };
                    let message = echo_agent_app_core::tasks::task_runtime::project_awaiter_surface_event(&event)
                        .map(|projection| projection.display_message())
                        .unwrap_or_else(|| "Awaiter result is unavailable".to_string());
                    yield ChannelOutboundDraft::ordinary(message);
                }
                ChannelRenderEvent::Driver(
                    ChatDriverEvent::AwaiterResultDeliveryStarted { .. }
                    | ChatDriverEvent::AwaiterResultAcknowledged { .. },
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
                ChannelRenderEvent::Driver(ChatDriverEvent::InputRequest { request_id, prompt }) => {
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
                    echo_agent_app_core::chat_driver::TurnOutcome::Completed,
                ) => {
                    flush_all!();
                    break;
                }
                ChannelRenderEvent::Terminal(
                    echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                ) => {
                    flush_all!();
                    yield ChannelOutboundDraft::terminal("[cancelled] The channel turn was cancelled.");
                    break;
                }
                ChannelRenderEvent::Terminal(
                    echo_agent_app_core::chat_driver::TurnOutcome::Failed(failure),
                ) => {
                    flush_all!();
                    yield ChannelOutboundDraft::terminal(format!("[failed:{}] {}", failure.code, failure.message));
                    break;
                }
                ChannelRenderEvent::Journaled(_) => {}
            }
        }
    };
    channel_outbound_transport(s.boxed(), channel_id, to, chat_type)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "channels")]
    use std::sync::Arc;
    #[cfg(feature = "channels")]
    struct SessionIdentityProbe;

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
    impl echo_agent_app_core::chat_driver::ChatSink for ChannelTestSink {
        fn on_event(&self, _event: echo_agent_app_core::chat_driver::ChatDriverEvent) -> bool {
            true
        }
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
        started: std::sync::atomic::AtomicBool,
        started_notify: tokio::sync::Notify,
    }

    #[cfg(feature = "channels")]
    impl ChannelCancellationBarrierLlmClient {
        fn new() -> Self {
            Self {
                started: std::sync::atomic::AtomicBool::new(false),
                started_notify: tokio::sync::Notify::new(),
            }
        }

        async fn wait_started(&self) {
            if self.started.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            self.started_notify.notified().await;
        }

        async fn wait_for_cancel(
            &self,
            request: &echo_agent::llm::ChatRequest,
        ) -> echo_agent::error::Result<()> {
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
                self.wait_for_cancel(&request).await?;
                Err(echo_agent::error::ReactError::Other(
                    "channel cancellation barrier returned unexpectedly".to_string(),
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
        use echo_agent_app_core::tasks::task_runtime::{RunTurnOrigin, TaskRunResumeIdentity};

        let identity = TaskRunResumeIdentity {
            run_id: "run-a".to_string(),
            workspace_id: "workspace-a".to_string(),
            conversation_id: "conversation-a".to_string(),
            root_message_id: "root-a".to_string(),
            created_at: chrono::Utc::now(),
            goal_revision: 3,
            journal_sequence: 11,
            continuation_enabled: true,
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
            checkpoint.messages_json = "[]".to_string();
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
        use echo_agent_app_core::foreground_turn::{
            ForegroundTurnControl, ForegroundTurnError, ForegroundTurnSettlement,
            ForegroundTurnSurface,
        };

        let control = ForegroundTurnControl::default();
        let settled = Ok(ForegroundTurnSettlement {
            workspace_id: "workspace-a".to_string(),
            surface: ForegroundTurnSurface::Channel,
            conversation_id: "conversation-a".to_string(),
            turn_id: "root-turn".to_string(),
            outcome: echo_agent_app_core::chat_driver::TurnOutcome::Completed,
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
        use echo_agent_app_core::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

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
            .settle_after_observers(echo_agent_app_core::chat_driver::TurnOutcome::Cancelled)
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
        use echo_agent_app_core::foreground_turn::{
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
                echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "planned_resume",
                        "injected planned resume failure",
                    ),
                ),
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
            echo_agent_app_core::chat_driver::TurnOutcome::Failed(_)
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
        use echo_agent_app_core::chat_event_log::{ChatEventLog, ChatEventRetention};
        use echo_agent_app_core::conversation_input::{
            ConversationInputAddress, ConversationInputPhase, ConversationInputService,
        };
        use echo_agent_app_core::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

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
            .settle_after_observers(echo_agent_app_core::chat_driver::TurnOutcome::Completed)
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            settlement.outcome,
            echo_agent_app_core::chat_driver::TurnOutcome::Failed(_)
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
        use echo_agent_app_core::foreground_turn::{ForegroundTurnSnapshot, ForegroundTurnSurface};

        let snapshot = ForegroundTurnSnapshot {
            workspace_id: "workspace-a".to_string(),
            surface: ForegroundTurnSurface::Channel,
            conversation_id: "conversation-a".to_string(),
            root_turn_id: "root-turn".to_string(),
            active_turn_id: "continuation-turn".to_string(),
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
        use echo_agent_app_core::foreground_turn::{ForegroundTurnSnapshot, ForegroundTurnSurface};

        let unrelated = ForegroundTurnSnapshot {
            workspace_id: "workspace-a".to_string(),
            surface: ForegroundTurnSurface::Gui,
            conversation_id: "sender-conversation".to_string(),
            root_turn_id: "gui-root".to_string(),
            active_turn_id: "gui-root".to_string(),
            cancellation_requested: false,
        };
        let extraction = ForegroundTurnSnapshot {
            workspace_id: "workspace-a".to_string(),
            surface: ForegroundTurnSurface::Channel,
            conversation_id: "sender-conversation".to_string(),
            root_turn_id: "extract-root".to_string(),
            active_turn_id: "extract-root".to_string(),
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
        use echo_agent_app_core::chat_driver::TurnOutcome;
        use echo_agent_app_core::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};
        use echo_agent_app_core::tasks::task_runtime::{
            RunTurnBinding, RunTurnOrigin, TaskRuntimeStore, TurnVisibility,
        };

        let temporary =
            std::env::temp_dir().join(format!("eko-channel-foreground-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temporary).map_err(|error| error.to_string())?;
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
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
        let turn = echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
            echo_agent_app_core::prepared_turn::UserTurnInput {
                text: "continue",
                attachments: &[],
                spill_dir: &temporary,
                conversation_id: Some("conversation-a"),
                turn_id: Some("root-turn"),
            },
        )
        .map_err(|error| error.to_string())?;
        let resources = std::sync::Arc::new(echo_agent_app_core::chat_resources::ChatResources {
            execution_scope: echo_agent_app_core::workspace::WorkspaceExecutionScope::workspace(
                &echo_agent_app_core::workspace::WorkspaceId::from_raw(workspace_id.clone()),
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
            run_id: None,
            turn_id: "continuation-turn".to_string(),
            root_message_id: "root-turn".to_string(),
            origin: RunTurnOrigin::Continuation,
            transcript_visibility: TurnVisibility::Visible,
            expected_resume: None,
        };
        let driver = tokio::spawn(async move {
            echo_agent_app_core::foreground_turn::drive_foreground_chat_turn(
                lease, &agent, &turn, resources, binding,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), llm.wait_started())
            .await
            .map_err(|_| "channel model request did not reach cancellation barrier".to_string())?;
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
        let _cleanup = std::fs::remove_dir_all(&temporary);
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn dropped_old_owner_keeps_replacement_in_real_active_map() -> Result<(), String> {
        let temporary =
            std::env::temp_dir().join(format!("eko-channel-owner-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temporary).map_err(|error| error.to_string())?;
        let agent = channel_test_agent(std::time::Duration::ZERO)?;
        let mcp_runtime = std::sync::Arc::new(
            echo_agent_app_core::mcp_config_runtime::McpConfigRuntime::from_snapshot(
                temporary.join("mcp.json"),
                echo_agent::mcp::McpConfigFile::default(),
            ),
        );
        let state = echo_agent_app_core::state::AppState::from_shared(
            agent,
            None,
            std::sync::Arc::new(echo_agent_app_core::hitl::HitlDispatcher::new()),
            None,
            None,
            Default::default(),
            mcp_runtime,
            echo_agent_app_core::product_data_io::ProductDataIoService::new(),
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
        for command in [
            "/skills list",
            "/plugins list",
            "/mcp list",
            "/hooks list",
            "/lsp status",
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
                kind: echo_agent_app_core::extension_commands::ExtensionKind::Browser,
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
        let registry = echo_agent_app_core::workspace::registry::WorkspaceRegistry::with_base_dir(
            temporary.join("registry"),
        )
        .map_err(|error| error.to_string())?;
        let workspace = registry
            .create_at(
                "channel-extension-scope",
                echo_agent_app_core::workspace::WorkspaceKind::General,
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
        for descriptor in
            echo_agent_app_core::developer_commands::DeveloperCommandRegistry::commands()
        {
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
        let mut stream = super::immediate_channel_response(&message, huge);
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

        let terminal = echo_agent_app_core::terminal::TerminalService::new();
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
    async fn terminal_stream_preserves_utf8_split_at_every_byte() -> Result<(), String> {
        use echo_agent::channels::{ChatType, InboundMessage};
        use echo_agent_app_core::terminal::{TerminalEvent, TerminalExitReason};
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
        use echo_agent::channels::{ChatType, InboundMessage};
        use echo_agent_app_core::terminal::{TerminalEvent, TerminalExitReason};
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
        let terminal = echo_agent_app_core::terminal::TerminalService::new();
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
        assert!(joined.contains("[REDACTED]"));
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
    -> Result<(), echo_agent_app_core::foreground_turn::ForegroundTurnError> {
        use echo_agent_app_core::chat_driver::TurnOutcome;
        use echo_agent_app_core::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

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
        use echo_agent_app_core::chat_driver::{ChatDriverEvent, ChatSink};

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
    fn large_token_is_utf8_chunked_and_cancels_at_bounded_queue() -> Result<(), String> {
        use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity};
        use echo_agent_app_core::chat_driver::{ChatDriverEvent, ChatSink};

        let (tx, mut rx) = tokio::sync::mpsc::channel(super::CHANNEL_EVENT_QUEUE_CAPACITY);
        let cancellation = echo_agent::agent::CancellationToken::new();
        let sink = super::ChannelSurfaceSink::new(tx, cancellation.clone());
        let identity = EventIdentity::new("channel-large-token", "turn-1")
            .map_err(|error| error.to_string())?;
        let envelope = EventEnvelope::new(
            &identity,
            1,
            None,
            AgentEvent::Token("中文🙂".repeat(25_000)),
        )
        .map_err(|error| error.to_string())?;
        assert!(!sink.on_event(ChatDriverEvent::Agent(Box::new(envelope))));
        assert!(cancellation.is_cancelled());
        assert_eq!(rx.len(), super::CHANNEL_EVENT_QUEUE_CAPACITY);
        while let Ok(event) = rx.try_recv() {
            let super::ChannelRenderEvent::Token(token) = event else {
                return Err("large token queued a non-token event".to_string());
            };
            assert!(token.len() <= super::CHANNEL_TOKEN_COALESCE_BYTES);
            assert!(token.is_char_boundary(token.len()));
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn terminal_drains_accepted_final_answer_before_publication() -> Result<(), String> {
        use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity};
        use echo_agent_app_core::chat_driver::{ChatDriverEvent, TurnOutcome};
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
            .send(TurnOutcome::Completed)
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
    async fn continuation_prompt_is_delivered_before_foreground_terminal() -> Result<(), String> {
        use echo_agent_app_core::chat_driver::TurnOutcome;
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
            .send(TurnOutcome::Completed)
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
            project_channel_input_lifecycle,
        };
        use echo_agent_app_core::chat_event_log::{ChatEventLog, ChatEventRetention};
        use echo_agent_app_core::conversation_input::{
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

        fn address() -> echo_agent_app_core::conversation_input::ConversationInputAddress {
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
                .settle_attempt(
                    &attempt,
                    &echo_agent_app_core::chat_driver::TurnOutcome::Completed,
                )
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
                    .settle_attempt(
                        &attempt,
                        &echo_agent_app_core::chat_driver::TurnOutcome::Completed,
                    )
                    .await
                    .map_err(|error| error.to_string())?
                    .duplicate
            );
            Ok(())
        }

        #[tokio::test]
        async fn canonical_pump_receipts_reach_channel_renderer_in_commit_order()
        -> Result<(), String> {
            use echo_agent_app_core::chat_driver::ChatDriverEvent;
            use echo_agent_app_core::conversation_input::ConversationInputFact;

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
            project_channel_input_lifecycle(
                log.as_ref(),
                &submitted.identity,
                &route.render_tx,
                route.lifecycle_cursor.as_ref(),
            )
            .await?;
            service
                .settle_attempt(
                    &attempt,
                    &echo_agent_app_core::chat_driver::TurnOutcome::Completed,
                )
                .await
                .map_err(|error| error.to_string())?;
            project_channel_input_lifecycle(
                log.as_ref(),
                &submitted.identity,
                &route.render_tx,
                route.lifecycle_cursor.as_ref(),
            )
            .await?;

            let mut completed_phases = Vec::new();
            while let Ok(event) = receiver.render_rx.try_recv() {
                if let ChannelRenderEvent::Driver(ChatDriverEvent::InputLifecycle(fact)) = event {
                    completed_phases.push(match fact.as_ref() {
                        ConversationInputFact::Persisted { .. } => "persisted",
                        ConversationInputFact::AttemptStarted { .. } => "attempt_started",
                        ConversationInputFact::MailboxAccepted { .. } => "mailbox_accepted",
                        ConversationInputFact::Drained { .. } => "drained",
                        ConversationInputFact::TurnSettled { .. } => "turn_settled",
                        ConversationInputFact::Deferred { .. } => "deferred",
                        ConversationInputFact::Reordered { .. } => "reordered",
                        ConversationInputFact::RecoveryRequired { .. } => "recovery_required",
                        ConversationInputFact::Cancelled { .. } => "cancelled",
                    });
                }
            }
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
            project_channel_input_lifecycle(
                log.as_ref(),
                &recovery.identity,
                &recovery_route.render_tx,
                recovery_route.lifecycle_cursor.as_ref(),
            )
            .await?;
            service
                .settle_attempt(
                    &recovery_attempt,
                    &echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "observer",
                            "injected observer failure",
                        ),
                    ),
                )
                .await
                .map_err(|error| error.to_string())?;
            project_channel_input_lifecycle(
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
            let spill_dir =
                echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(root));
            let turn = echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
                echo_agent_app_core::prepared_turn::UserTurnInput {
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
            let product_data_io = echo_agent_app_core::product_data_io::ProductDataIoService::new();
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
                    workspace_io_receipt:
                        echo_agent_app_core::state::ScopedWorkspaceIoReceipt::global_for_test(
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

            let spill_a = echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(
                &workspace_a,
            ));
            let spill_b = echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(
                &workspace_b,
            ));
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
                let spill_dir =
                    echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(root));
                let turn = echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
                    echo_agent_app_core::prepared_turn::UserTurnInput {
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
        use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity, ToolInvocation};
        use echo_agent::channels::{ChatType, OutboundMessage};
        use echo_agent::error::Result;
        use echo_agent::tools::{
            ToolFailureCategory, ToolOutputChannel, ToolResult, ToolStreamEvent,
        };
        use echo_agent_app_core::tool_execution::{
            ToolExecutionOwner, ToolExecutionStatus, ToolExecutionSummary,
        };
        use echo_agent_app_core::tool_execution_projection::{
            ToolExecutionProjectionKind, ToolExecutionProjectionUpdate,
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
                        ChannelRenderEvent::Driver(
                            echo_agent_app_core::chat_driver::ChatDriverEvent::Agent(Box::new(
                                envelope,
                            )),
                        )
                    })
                })
            }))
            .boxed()
        }

        fn agent_render_event(sequence: u64, payload: AgentEvent) -> Result<ChannelRenderEvent> {
            let identity = EventIdentity::new("channel-tool-stream", "channel-tool-turn")?;
            EventEnvelope::new(&identity, sequence, None, payload).map(|envelope| {
                ChannelRenderEvent::Driver(
                    echo_agent_app_core::chat_driver::ChatDriverEvent::Agent(Box::new(envelope)),
                )
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
        async fn flush_on_newline() {
            // Token("a") Token("b\n") Token("c") FinalAnswer("") → "ab\n", "c"
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("a".into())),
                Ok(AgentEvent::Token("b\n".into())),
                Ok(AgentEvent::Token("c".into())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts.concat(), "ab\nc");
        }

        #[tokio::test]
        async fn flush_on_sentence_end() {
            // 中文句末标点 。 触发 flush
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("你好。".into())),
                Ok(AgentEvent::Token("再见".into())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts.concat(), "你好。再见");
        }

        #[tokio::test]
        async fn flush_on_threshold() {
            // 超过 FLUSH_THRESHOLD 字符阈值 flush(单 Token 阈值+10)
            let n = FLUSH_THRESHOLD + 10;
            let long: String = "x".repeat(n);
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token(long.clone())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts.len(), 1, "threshold flush yields 1");
            assert_eq!(texts.first().map(|text| text.chars().count()), Some(n));
        }

        #[tokio::test]
        async fn finalanswer_flushes_remaining() {
            // 无标点的短串 + FinalAnswer flush 剩余
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("hi".into())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts, vec!["hi".to_string()]);
        }

        #[tokio::test]
        async fn empty_buf_finalanswer_no_yield() {
            // FinalAnswer 前无 Token → 不 yield 空
            let evs = events_to_stream(vec![Ok(AgentEvent::FinalAnswer(String::new()))]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert!(texts.is_empty(), "no token before FinalAnswer → no yield");
        }

        #[tokio::test]
        async fn cancelled_terminal_is_user_visible() {
            let driver = events_to_stream(vec![
                Ok(AgentEvent::Token("partial".into())),
                Ok(AgentEvent::Cancelled),
            ]);
            let terminal = futures::stream::once(async {
                Ok(ChannelRenderEvent::Terminal(
                    echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
                ))
            });
            let evs = driver.chain(terminal).boxed();
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(
                texts,
                vec![
                    "partial".to_string(),
                    "[cancelled] The channel turn was cancelled.".to_string()
                ]
            );
        }

        #[tokio::test]
        async fn failed_terminal_is_typed_and_user_visible() -> std::result::Result<(), String> {
            let driver = events_to_stream(vec![
                Ok(AgentEvent::Token("partial".into())),
                Ok(AgentEvent::error_message("llm", "boom")),
            ]);
            let terminal = futures::stream::once(async {
                Ok(ChannelRenderEvent::Terminal(
                    echo_agent_app_core::chat_driver::TurnOutcome::Failed(
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
            assert_eq!(second.text, "[failed:llm_network] boom");
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
                    echo_agent_app_core::chat_driver::TurnOutcome::Cancelled,
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
                .position(|text| text.starts_with("[cancelled]"))
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
                echo_agent_app_core::tool_execution::ToolExecutionRepository::open(
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
                    echo_agent_app_core::chat_driver::TurnOutcome::Completed,
                )),
            ]);
            let output = super::super::aggregate_by_sentence_with_repository(
                events,
                "qq".to_string(),
                "u1".to_string(),
                ChatType::Direct,
                repository,
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
            use echo_agent_app_core::tasks::task_runtime::RuntimeEventKind;
            use echo_agent_app_core::tasks::task_runtime::executor::ExecEvent;

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
                    echo_agent_app_core::chat_driver::ChatDriverEvent::Execution(
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
                    ),
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
                    echo_agent_app_core::chat_driver::ChatDriverEvent::Execution(
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
                    ),
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
                    echo_agent_app_core::chat_driver::ChatDriverEvent::Execution(
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
                    ),
                )),
                Ok(ChannelRenderEvent::Terminal(
                    echo_agent_app_core::chat_driver::TurnOutcome::Completed,
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
            use echo_agent_app_core::tasks::task_runtime::RuntimeEventKind;
            use echo_agent_app_core::tasks::task_runtime::executor::ExecEvent;

            let secret = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
            let sensitive = format!("Bearer {secret}");
            let mut events = vec![
                Ok(ChannelRenderEvent::Prompt(format!("prompt {sensitive}"))),
                Ok(ChannelRenderEvent::Token(format!(
                    "huge token {sensitive} {}",
                    "中文🙂".repeat(5_000)
                ))),
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
                    echo_agent_app_core::chat_driver::ChatDriverEvent::Execution(ExecEvent::run(
                        "workspace-a",
                        "conversation-a",
                        "run-a",
                        RuntimeEventKind::RunFailed,
                        serde_json::json!({"token": secret}),
                    )),
                )),
                Ok(ChannelRenderEvent::Driver(
                    echo_agent_app_core::chat_driver::ChatDriverEvent::Interrupt {
                        run_id: format!("run-{sensitive}"),
                        goal: format!("goal {sensitive}"),
                        new_message: format!("message {sensitive}"),
                    },
                )),
                Ok(ChannelRenderEvent::Driver(
                    echo_agent_app_core::chat_driver::ChatDriverEvent::ApprovalRequest {
                        request_id: format!("approval-{sensitive}"),
                        tool_name: format!("tool-{sensitive}"),
                        args: serde_json::json!({"token": secret}),
                        prompt: format!("approve {sensitive}"),
                    },
                )),
                Ok(ChannelRenderEvent::Driver(
                    echo_agent_app_core::chat_driver::ChatDriverEvent::InputRequest {
                        request_id: format!("input-{sensitive}"),
                        prompt: format!("input {sensitive}"),
                    },
                )),
                Ok(ChannelRenderEvent::Driver(
                    echo_agent_app_core::chat_driver::ChatDriverEvent::SelectionRequest {
                        request_id: format!("selection-{sensitive}"),
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
                echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message(
                        "terminal_failure",
                        format!("terminal {sensitive}"),
                    ),
                ),
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
            assert!(terminal.starts_with("[failed:terminal_failure]"));
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
            // 中文 + emoji 不 panic,按 FinalAnswer flush
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("你好🦀世界".into())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts, vec!["你好🦀世界".to_string()]);
        }

        #[tokio::test]
        async fn fullwidth_punctuation_flushes() {
            // 全角 ！ ？ 。 触发 flush(验证 is_sentence_end 全角分支)
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("第一句！".into())),
                Ok(AgentEvent::Token("第二句？".into())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts.concat(), "第一句！第二句？");
        }
    }
}
