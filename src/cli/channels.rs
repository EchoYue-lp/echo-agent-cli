//! 应用层 IM channel 消息处理器 —— 把 IM 消息桥接到 `AgentPool`。
//!
//! 框架层 `AgentChannelHandler::from_config` 要求调用方显式传入 `LlmConfig`，而 EKO
//! channel 不直接构造该 handler。此处 agent 从 `AgentPool::acquire` 取，经
//! `AgentRuntime::bootstrap` 全套接通
//! （state_store / store / compressor / MemoryLayerManager / permission_service /
//! cache_user_id / conversation_id）。会话按平台 conversation 隔离，群聊不会按 sender
//! 交叉复用上下文。
//!
//! 归属（spec §D1-6）：`AgentPool` 是 EKO 产品概念，handler 放应用层（bin crate），
//! 不进框架 `channels.rs`。框架复用方可按需使用要求显式 LLM 依赖的
//! `AgentChannelHandler::from_config` / `from_config_with_client`。

#[cfg(feature = "channels")]
use std::sync::Arc;

#[cfg(feature = "channels")]
use echo_agent_app_core::agent_pool::AgentPool;

#[cfg(feature = "channels")]
use echo_agent_app_core::foreground_turn::{
    ForegroundTurnControl, ForegroundTurnError, ForegroundTurnSurface,
};

#[cfg(feature = "channels")]
use echo_agent_app_core::hitl::{ChannelHumanLoopProvider, ChannelHumanLoopResolution};

#[cfg(feature = "channels")]
enum ChannelRenderEvent {
    Driver(echo_agent_app_core::chat_driver::ChatDriverEvent),
    Prompt(String),
    Terminal(echo_agent_app_core::chat_driver::TurnOutcome),
}

#[cfg(feature = "channels")]
enum ChannelTaskRunControl {
    Reply(String),
    Resume {
        run_id: String,
        root_message_id: String,
        expected_resume: echo_agent_app_core::tasks::task_runtime::TaskRunResumeIdentity,
        continuation_enabled: bool,
        runtime: Box<echo_agent_app_core::state::ScopedChatRuntime>,
    },
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
    mut driver_rx: tokio::sync::mpsc::UnboundedReceiver<
        echo_agent_app_core::chat_driver::ChatDriverEvent,
    >,
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
                    Some(event) => yield Ok(ChannelRenderEvent::Driver(event)),
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

/// IM channel 消息处理器：持 `AgentPool`，每 `handle` 从 pool 取/复用 per-sender agent。
///
/// TUI/GUI functional parity (AGENTS.md): channels drive chat through the
/// shared foreground driver. Holds the per-sender `AgentPool` + the
/// `TaskRuntimeStore` (so `create_complex_task` can build `ChatResources`).
/// Whether a complex run is warranted is decided by the agent itself, not
/// pre-judged here.
#[cfg(feature = "channels")]
pub struct AppChannelMessageHandler {
    app_state: Arc<echo_agent_app_core::state::AppState>,
    webhook_emitter: Arc<echo_agent_app_core::webhook::WebhookEmitter>,
    hitl: Arc<ChannelHumanLoopProvider>,
    foreground_turns: ForegroundTurnControl,
    interaction_mode:
        tokio::sync::RwLock<echo_agent_app_core::tasks::task_runtime::InteractionMode>,
}

#[cfg(feature = "channels")]
impl AppChannelMessageHandler {
    pub fn new(
        app_state: Arc<echo_agent_app_core::state::AppState>,
        _pool: Arc<AgentPool>,
        _store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
        _review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
        webhook_emitter: Arc<echo_agent_app_core::webhook::WebhookEmitter>,
        foreground_turns: ForegroundTurnControl,
    ) -> Self {
        Self {
            app_state,
            webhook_emitter,
            hitl: Arc::new(ChannelHumanLoopProvider::new()),
            foreground_turns,
            interaction_mode: tokio::sync::RwLock::new(
                echo_agent_app_core::tasks::task_runtime::InteractionMode::Auto,
            ),
        }
    }

    /// Per-conversation pool key.
    fn conversation_id(channel_id: &str, chat_id: &str) -> String {
        format!("channel:{channel_id}:{chat_id}")
    }

    /// Per-conversation provider cache identity.
    fn cache_user_id(channel_id: &str, chat_id: &str) -> String {
        sanitize_cache_user_id(&format!("im-{channel_id}-{chat_id}"))
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

    fn current_task_run(
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
        let run = match requested_run_id.filter(|run_id| !run_id.trim().is_empty()) {
            Some(run_id) => store.get_run(run_id).map_err(|error| error.to_string())?,
            None => store
                .latest_run_for_conversation(conv)
                .map_err(|error| error.to_string())?,
        }
        .ok_or_else(|| "No TaskRun was found for this conversation.".to_string())?;
        if run.conversation_id != conv {
            return Err(format!(
                "TaskRun {} belongs to another conversation.",
                run.run_id
            ));
        }
        let snapshot = store
            .get_run_state(&run.run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("TaskRun {} has no event projection.", run.run_id))?;
        Ok((store, snapshot))
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
        let runtime = match self.app_state.current_chat_runtime().await {
            Ok(runtime) => runtime,
            Err(error) => {
                return Some(ChannelTaskRunControl::Reply(format!(
                    "Workspace runtime is unavailable: {error}"
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
            ) {
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
                    service.queue_guidance(
                        parsed.identity,
                        instruction,
                        echo_agent_app_core::tasks::task_runtime::SubagentControlActorSource::Channel,
                    )
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
            ) {
                Ok(value) => value,
                Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
            };
            let reply = match store.update_run_goal(
                &snapshot.run.run_id,
                parsed.expected_goal_revision,
                &parsed.new_goal,
                &parsed.reason,
                echo_agent_app_core::tasks::task_runtime::RunGoalActorSource::Channel,
            ) {
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
                match Self::current_task_run(task_runtime.clone(), conv, requested_run_id) {
                    Ok(value) => value,
                    Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
                };
            let reply = match store.completion_gate_report(&snapshot.run.run_id) {
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
            ) {
                Ok(value) => value,
                Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
            };
            let reply = match store.skip_goal_requirement(
                &snapshot.run.run_id,
                parsed.expected_goal_revision,
                &parsed.requirement_id,
                &parsed.reason,
                echo_agent_app_core::tasks::task_runtime::RunGoalActorSource::Channel,
            ) {
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
        let (store, snapshot) = match Self::current_task_run(task_runtime, conv, requested_run_id) {
            Ok(value) => value,
            Err(error) => return Some(ChannelTaskRunControl::Reply(error)),
        };
        let run_id = snapshot.run.run_id.clone();
        let reply = match action {
            "status" => format_channel_task_run_status(&snapshot),
            "pause" => match store.request_pause(&run_id) {
                Ok(true) => format!("TaskRun {run_id} paused."),
                Ok(false) => format!("TaskRun {run_id} is not actively pausable."),
                Err(error) => format!("Unable to pause TaskRun {run_id}: {error}"),
            },
            "cancel" => match store.request_cancel(&run_id) {
                Ok(true) => format!("TaskRun {run_id} cancelled."),
                Ok(false) => format!("TaskRun {run_id} is already terminal."),
                Err(error) => format!("Unable to cancel TaskRun {run_id}: {error}"),
            },
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
                        run_id,
                        root_message_id: snapshot.run.root_message_id.clone(),
                        expected_resume:
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
                match budgets.and_then(|(tokens, time)| {
                    store
                        .update_run_continuation_budgets(&run_id, tokens, time)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(_) => format!("TaskRun {run_id} budgets updated."),
                    Err(error) => format!("Unable to update TaskRun {run_id} budgets: {error}"),
                }
            }
            _ => "Unsupported TaskRun command.".to_string(),
        };
        Some(ChannelTaskRunControl::Reply(reply))
    }

    async fn control_command_response(&self, message: &str, conv: &str) -> Option<String> {
        let mut parts = message.trim().splitn(2, char::is_whitespace);
        let command = parts.next()?;
        let argument = parts.next().map(str::trim).unwrap_or_default();
        let scoped_runtime = if matches!(command, "/stop" | "/reset" | "/steer") {
            match self.app_state.current_chat_runtime().await {
                Ok(runtime) => Some(runtime),
                Err(error) => {
                    return Some(format!("Workspace runtime is unavailable: {error}"));
                }
            }
        } else {
            None
        };
        match command {
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
                Some(
                    match self
                        .app_state
                        .compress_conversation_owned(
                            echo_agent_app_core::manual_compression::ManualCompressionRequest {
                                workspace_id: self
                                    .app_state
                                    .current_execution_scope()
                                    .await
                                    .workspace_id()
                                    .to_string(),
                                conversation_id: conv.to_string(),
                                surface: ForegroundTurnSurface::Channel,
                                focus,
                                keep_messages,
                            },
                        )
                        .await
                    {
                        Ok(receipt) => format!(
                            "Context compressed: {} -> {} messages, {} tokens saved.",
                            receipt.messages_before,
                            receipt.messages_after,
                            receipt.tokens_saved()
                        ),
                        Err(error) => format!("Context compression failed: {error}"),
                    },
                )
            }
            "/stop" => {
                let runtime = scoped_runtime.as_ref()?;
                let Some(snapshot) = self.foreground_turns.snapshot_scoped(
                    runtime.execution_scope().workspace_id(),
                    ForegroundTurnSurface::Channel,
                    conv,
                ) else {
                    return Some("No active channel turn to stop.".to_string());
                };
                Some(
                    match self
                        .foreground_turns
                        .root_cancel_and_wait_scoped(
                            runtime.execution_scope().workspace_id(),
                            ForegroundTurnSurface::Channel,
                            conv,
                            &snapshot.root_turn_id,
                        )
                        .await
                    {
                        Ok(settlement) => format!(
                            "Turn {} settled as {}.",
                            settlement.turn_id,
                            settlement.outcome.status()
                        ),
                        Err(ForegroundTurnError::NoActiveTurn { .. }) => {
                            "The channel turn already settled.".to_string()
                        }
                        Err(error) => format!("Unable to stop the active turn: {error}"),
                    },
                )
            }
            "/cancel" => Some(match self.hitl.reject_front("Cancelled by user").await {
                ChannelHumanLoopResolution::NoPending => {
                    "No pending approval or input request to cancel.".to_string()
                }
                ChannelHumanLoopResolution::Resolved(message)
                | ChannelHumanLoopResolution::Invalid(message) => message,
            }),
            "/reset" => {
                let runtime = scoped_runtime.as_ref()?;
                if let Some(snapshot) = self.foreground_turns.snapshot_scoped(
                    runtime.execution_scope().workspace_id(),
                    ForegroundTurnSurface::Channel,
                    conv,
                ) && let Err(error) = self
                    .foreground_turns
                    .root_cancel_and_wait_scoped(
                        runtime.execution_scope().workspace_id(),
                        ForegroundTurnSurface::Channel,
                        conv,
                        &snapshot.root_turn_id,
                    )
                    .await
                    && !matches!(error, ForegroundTurnError::NoActiveTurn { .. })
                {
                    return Some(format!(
                        "Unable to reset before the active turn settles: {error}"
                    ));
                }
                let reset_turn_id = uuid::Uuid::new_v4().to_string();
                let reset_lease = match runtime
                    .begin_turn(
                        &self.foreground_turns,
                        ForegroundTurnSurface::Channel,
                        conv,
                        reset_turn_id,
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
                let generation_receipts = match Self::generation_receipts(runtime) {
                    Ok(receipts) => receipts,
                    Err(message) => {
                        reset_lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                            echo_agent::error::AgentFailure::message(
                                "workspace_generation",
                                message.clone(),
                            ),
                        ));
                        return Some(message);
                    }
                };
                let Some(pool) = runtime.pool() else {
                    reset_lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "agent_pool",
                            "AgentPool is unavailable",
                        ),
                    ));
                    generation_receipts.release_lifo();
                    return Some("AgentPool is unavailable".to_string());
                };
                let hitl = Arc::clone(&self.hitl);
                let conv_owned = conv.to_string();
                let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                if let Err(error) =
                    self.foreground_turns
                        .supervise(reset_lease, move |reset_lease| async move {
                            hitl.reject_all("Conversation reset by user").await;
                            let retirement = match pool.lease_existing(&conv_owned).await {
                                Ok(Some(execution)) => {
                                    pool.retire_execution(&conv_owned, execution).await
                                }
                                Ok(None) => Ok(false),
                                Err(error) => Err(error),
                            };
                            let (outcome, message) = match retirement {
                                Ok(_) => (
                                    echo_agent_app_core::chat_driver::TurnOutcome::Completed,
                                    "Conversation reset after exact foreground settlement."
                                        .to_string(),
                                ),
                                Err(error) => (
                                    echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                                        echo_agent::error::AgentFailure::message(
                                            "channel_reset",
                                            error.to_string(),
                                        ),
                                    ),
                                    format!("Unable to retire the conversation agent: {error}"),
                                ),
                            };
                            generation_receipts.release_lifo();
                            reset_lease.settle(outcome);
                            let _delivered = result_tx.send(message);
                        })
                {
                    return Some(format!("Unable to supervise reset settlement: {error}"));
                }
                Some(result_rx.await.unwrap_or_else(|_| {
                    "Reset owner ended without publishing its terminal result.".to_string()
                }))
            }
            "/steer" => {
                if argument.is_empty() {
                    return Some("Usage: /steer <additional instruction>".to_string());
                }
                let runtime = scoped_runtime.as_ref()?;
                let Some(snapshot) = self.foreground_turns.snapshot_scoped(
                    runtime.execution_scope().workspace_id(),
                    ForegroundTurnSurface::Channel,
                    conv,
                ) else {
                    return Some("No active channel turn to steer.".to_string());
                };
                let Some(pool) = runtime.pool() else {
                    return Some("The active workspace has no AgentPool.".to_string());
                };
                let execution = match pool.lease_existing(conv).await {
                    Ok(Some(execution)) => execution,
                    Ok(None) => {
                        return Some("The active channel turn has no attached agent.".to_string());
                    }
                    Err(error) => {
                        return Some(format!(
                            "Unable to access the active channel agent: {error}"
                        ));
                    }
                };
                let agent = execution.agent();
                let response = match agent
                    .steer_input(
                        Some(&snapshot.active_turn_id),
                        echo_agent::prelude::Message::user(argument.to_string()),
                    )
                    .await
                {
                    Ok(turn_id) => format!("Additional instruction accepted for {turn_id}."),
                    Err(echo_agent::agent::TurnSteerError::NotSteerable { .. }) => {
                        "The active turn is not currently steerable; try again shortly.".to_string()
                    }
                    Err(echo_agent::agent::TurnSteerError::NoActiveTurn) => {
                        "The channel turn already settled.".to_string()
                    }
                    Err(error) => format!("Unable to steer the active turn: {error}"),
                };
                drop(execution);
                Some(response)
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
fn immediate_channel_response<'a>(
    msg: &echo_agent::channels::InboundMessage,
    message: impl Into<String>,
) -> futures::stream::BoxStream<'a, echo_agent::error::Result<echo_agent::channels::OutboundMessage>>
{
    use futures::StreamExt;

    let outbound = echo_agent::channels::OutboundMessage::new(
        &msg.channel_id,
        msg.reply_target(),
        msg.chat_type,
        message,
    );
    futures::stream::once(async move { Ok(outbound) }).boxed()
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
        Some("/trace" | "/analysis" | "/papers" | "/skills")
    )
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
fn channel_terminal_stream(
    message: &echo_agent::channels::InboundMessage,
    initial: String,
    mut events: tokio::sync::broadcast::Receiver<echo_agent_app_core::terminal::TerminalEvent>,
    terminal_id: String,
) -> futures::stream::BoxStream<
    'static,
    echo_agent::error::Result<echo_agent::channels::OutboundMessage>,
> {
    use futures::StreamExt;

    let channel_id = message.channel_id.clone();
    let reply_target = message.reply_target().to_string();
    let chat_type = message.chat_type;
    async_stream::stream! {
        yield Ok(echo_agent::channels::OutboundMessage::new(
            &channel_id,
            &reply_target,
            chat_type,
            initial,
        ));
        loop {
            match events.recv().await {
                Ok(echo_agent_app_core::terminal::TerminalEvent::Output { id, bytes })
                    if id == terminal_id =>
                {
                    let stripped = strip_ansi_escapes::strip(bytes);
                    let text = String::from_utf8_lossy(&stripped).into_owned();
                    if !text.is_empty() {
                        yield Ok(echo_agent::channels::OutboundMessage::new(
                            &channel_id,
                            &reply_target,
                            chat_type,
                            text,
                        ));
                    }
                }
                Ok(echo_agent_app_core::terminal::TerminalEvent::Exited { id, reason })
                    if id == terminal_id =>
                {
                    yield Ok(echo_agent::channels::OutboundMessage::new(
                        &channel_id,
                        &reply_target,
                        chat_type,
                        format!("Terminal '{terminal_id}' exited: {reason:?}"),
                    ));
                    break;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    yield Ok(echo_agent::channels::OutboundMessage::new(
                        &channel_id,
                        &reply_target,
                        chat_type,
                        format!(
                            "Terminal '{terminal_id}' output lagged by {skipped} event(s); subsequent output remains live."
                        ),
                    ));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    .boxed()
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
        Ok(echo_agent::channels::OutboundMessage::new(
            channel_id, to, chat_type, reply,
        ))
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
        let developer_command = match parse_developer_command(&msg.text) {
            Ok(command) => command,
            Err(error) => return Ok(immediate_channel_response(&msg, error)),
        };
        if let Some((command, args)) = developer_command {
            // Subscribe before dispatch so a fast shell cannot exit before
            // this channel starts observing its output.
            let terminal_events = self.app_state.terminal.subscribe();
            let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            let registry = echo_agent_app_core::developer_commands::DeveloperCommandRegistry::new(
                self.app_state.terminal.clone(),
                self.app_state.plugin_runtime.clone(),
            );
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
        let conv = Self::conversation_id(&msg.channel_id, msg.conversation_id());
        let cache_id = Self::cache_user_id(&msg.channel_id, msg.conversation_id());
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
                    run_id,
                    root_message_id,
                    expected_resume,
                    continuation_enabled,
                    runtime,
                } => {
                    resume_task_run = Some((
                        run_id,
                        root_message_id,
                        expected_resume,
                        continuation_enabled,
                        runtime,
                    ));
                }
            }
        }
        // Product control commands always outrank HITL parsing. `/stop` owns
        // turn cancellation; `/cancel` rejects only the queue front.
        if let Some(message) = self.control_command_response(&msg.text, &conv).await {
            return Ok(immediate_channel_response(&msg, message));
        }
        if msg.text.split_whitespace().next() == Some("/extract") {
            let workspace_id = self
                .app_state
                .current_execution_scope()
                .await
                .workspace_id()
                .to_string();
            let command = msg
                .text
                .trim()
                .strip_prefix("/extract")
                .map(str::trim)
                .unwrap_or_default();
            let message = self
                .app_state
                .execute_structured_extraction_command_for_scope(
                    &workspace_id,
                    &conv,
                    ForegroundTurnSurface::Channel,
                    command,
                )
                .await
                .unwrap_or_else(|error| format!("Structured extraction command failed: {error}"));
            return Ok(immediate_channel_response(&msg, message));
        }
        if self.hitl.has_pending().await {
            match self.hitl.resolve_message(&msg.text).await {
                ChannelHumanLoopResolution::Resolved(message)
                | ChannelHumanLoopResolution::Invalid(message) => {
                    return Ok(immediate_channel_response(&msg, message));
                }
                ChannelHumanLoopResolution::NoPending => {}
            }
        }
        if msg.text.split_whitespace().next() == Some("/mode") {
            let command_id = uuid::Uuid::new_v4().to_string();
            let (_runtime, lease) = match self
                .app_state
                .begin_scoped_chat_turn_owned(
                    ForegroundTurnSurface::Channel,
                    &conv,
                    command_id,
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
                        format!("Turn {active_turn_id} is still running; mode was not changed."),
                    ));
                }
                Err(error) => {
                    return Ok(immediate_channel_response(
                        &msg,
                        format!("Unable to admit the mode command: {error}"),
                    ));
                }
            };
            let message = parse_channel_mode_command(&msg.text, &self.interaction_mode)
                .await
                .unwrap_or_else(|| "Usage: /mode chat|task|auto".to_string());
            lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Completed);
            return Ok(immediate_channel_response(&msg, message));
        }

        // Management commands use the same exact foreground admission and
        // TaskRuntime -> pool order as chat. They do not mutate the agent when
        // any admission step fails.
        if is_agent_management_command(&msg.text) {
            let command_id = uuid::Uuid::new_v4().to_string();
            let (runtime, lease) = match self
                .app_state
                .begin_scoped_chat_turn_owned(
                    ForegroundTurnSurface::Channel,
                    &conv,
                    command_id,
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
            let generation_receipts = match Self::generation_receipts(&runtime) {
                Ok(receipts) => receipts,
                Err(message) => {
                    lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message(
                            "workspace_generation",
                            message.clone(),
                        ),
                    ));
                    return Ok(immediate_channel_response(&msg, message));
                }
            };
            let pool_execution = match runtime.agent_for(&conv).await {
                Ok(execution) => execution,
                Err(error) => {
                    generation_receipts.release_lifo();
                    let message = format!("AgentPool admission failed: {error}");
                    lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                        echo_agent::error::AgentFailure::message("agent_pool", message.clone()),
                    ));
                    return Ok(immediate_channel_response(&msg, message));
                }
            };
            let agent = pool_execution.agent();
            configure_channel_agent(&agent, &cache_id, Arc::clone(&self.hitl)).await;
            let message = if let Some(message) = channel_trace_response(&agent, &msg.text).await {
                message
            } else if let Some(message) = channel_analysis_response(&agent, &msg.text).await {
                message
            } else if let Some(message) = channel_papers_response(&agent, &msg.text).await {
                message
            } else if let Some(message) = channel_skills_response(&agent, &msg.text).await {
                message
            } else {
                "Unsupported channel management command.".to_string()
            };
            drop(pool_execution);
            generation_receipts.release_lifo();
            lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Completed);
            return Ok(immediate_channel_response(&msg, message));
        }

        if resume_task_run.is_some() && !msg.attachments.is_empty() {
            return Ok(immediate_channel_response(
                &msg,
                "TaskRun resume does not accept new attachments; send them in a separate turn.",
            ));
        }
        let turn_id = uuid::Uuid::new_v4().to_string();
        let admission = match resume_task_run.as_ref() {
            Some((_, _, _, _, runtime)) => runtime
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
                    format!(
                        "Turn {active_turn_id} is still running. Use /steer <instruction> or /stop."
                    ),
                ));
            }
            Err(error) => {
                return Ok(immediate_channel_response(
                    &msg,
                    format!("Foreground turn admission failed: {error}"),
                ));
            }
        };
        let stream_cancel = foreground_lease.cancellation_token();
        let text = resume_task_run.as_ref().map_or_else(
            || msg.text.clone(),
            |(run_id, _, _, _, _)| {
                format!(
                    "Resume the existing TaskRun {run_id} toward its unchanged Goal. Reload the authoritative TaskRuntime projection and continue the next useful work."
                )
            },
        );
        // Persist IM attachments into the same durable reference contract as
        // GUI/TUI so TaskRuntime subagents can reconstruct the same message.
        let attachment_refs = match stage_channel_attachments(&msg.attachments) {
            Ok(attachments) => attachments,
            Err(error) => {
                foreground_lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message("attachment_staging", error.clone()),
                ));
                return Ok(immediate_channel_response(
                    &msg,
                    format!("附件保存失败，未发送本条消息：{error}"),
                ));
            }
        };
        // Channels have no workspace root; long pastes spill to the global
        // user-input artifact dir (~/.eko/artifacts/user-input/).
        let spill_dir = echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(None);
        let interaction_mode = if resume_task_run.is_some() {
            echo_agent_app_core::tasks::task_runtime::InteractionMode::Task
        } else {
            *self.interaction_mode.read().await
        };
        let runtime_authored = resume_task_run.is_some();
        let turn = match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
            echo_agent_app_core::prepared_turn::UserTurnInput {
                text: &text,
                attachments: &attachment_refs,
                spill_dir: &spill_dir,
                conversation_id: Some(&conv),
                turn_id: Some(&turn_id),
            },
        ) {
            Ok(turn) if runtime_authored => turn.runtime_authored(),
            Ok(turn) => turn,
            Err(error) => {
                tracing::warn!(%error, conv = %conv, "channel user-turn preparation failed");
                foreground_lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message("prepared_turn", error.to_string()),
                ));
                let cleanup = echo_agent_app_core::attachments::discard_staged_attachment_refs(
                    &attachment_refs,
                )
                .err();
                let suffix = cleanup
                    .map(|cleanup| format!("；临时附件清理也失败：{cleanup}"))
                    .unwrap_or_default();
                return Ok(immediate_channel_response(
                    &msg,
                    format!("无法安全保存这条消息，请检查本地磁盘后重试：{error}{suffix}"),
                ));
            }
        };

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            echo_agent_app_core::chat_driver::ChatDriverEvent,
        >();
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        let Some(pool) = scoped_runtime.pool() else {
            foreground_lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(
                    "agent_pool",
                    "The active workspace has no AgentPool",
                ),
            ));
            return Ok(immediate_channel_response(
                &msg,
                "The active workspace has no AgentPool.",
            ));
        };
        let store = scoped_runtime.task_runtime();
        let execution_scope = scoped_runtime.execution_scope().clone();
        let app_state = self.app_state.clone();
        let webhook_emitter = self.webhook_emitter.clone();
        let review_integration = scoped_runtime.review_integration();
        let hitl = Arc::clone(&self.hitl);
        let prompt_rx = self.hitl.subscribe_prompts();
        let conv_owned = conv.clone();
        let planned_resume = resume_task_run.as_ref().and_then(
            |(_, _, expected_resume, continuation_enabled, _)| {
                (!continuation_enabled).then(|| expected_resume.clone())
            },
        );
        let explicit_binding =
            resume_task_run.and_then(|(_, _, expected_resume, continuation_enabled, _)| {
                continuation_enabled.then(|| {
                    echo_agent_app_core::tasks::task_runtime::RunTurnBinding::resume_expected(
                        expected_resume,
                        turn_id.clone(),
                    )
                })
            });
        let supervision =
            self.foreground_turns
                .supervise(foreground_lease, move |foreground_lease| async move {
                    use echo_agent_app_core::chat_driver::ChannelChatSink;
                    use echo_agent_app_core::foreground_turn::{
                        drive_foreground_pooled_chat, drive_foreground_pooled_chat_turn,
                    };

                    let renderer: std::sync::Arc<dyn echo_agent_app_core::chat_driver::ChatSink> =
                        std::sync::Arc::new(ChannelChatSink::new(tx));
                    let sink = echo_agent_app_core::chat_event_log::bind_surface_chat_sink(
                        echo_agent_app_core::chat_event_log::ChatSurface::Channel,
                        renderer,
                        app_state.storage.chat_events.clone(),
                        app_state.storage.tool_executions.clone(),
                        execution_scope.workspace_id().to_string(),
                        Some(conv_owned.clone()),
                        turn_id.clone(),
                    );
                    let res =
                        std::sync::Arc::new(echo_agent_app_core::chat_resources::ChatResources {
                            execution_scope,
                            pool: Some(pool.clone()),
                            store,
                            sink,
                            webhook_emitter: Some(webhook_emitter),
                            conv_id: Some(conv_owned.clone()),
                            root_message_id: turn_id,
                            attachments: turn.inline_attachment_refs(),
                            cancel: foreground_lease.cancellation_token(),
                            interaction_mode,
                            review_integration,
                            layer_manager: None,
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
                        let execution = match pool.acquire(&conv_owned).await {
                            Ok(execution) => execution,
                            Err(error) => {
                                let outcome = echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                                    echo_agent::error::AgentFailure::message(
                                        "agent_pool",
                                        error.to_string(),
                                    ),
                                );
                                foreground_lease.settle(outcome.clone());
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
                            foreground_lease.settle(outcome.clone());
                            outcome
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
                            foreground_lease.settle(outcome.clone());
                            outcome
                        }
                    } else {
                        match explicit_binding {
                            Some(binding) => {
                                drive_foreground_pooled_chat_turn(
                                    foreground_lease,
                                    pool,
                                    conv_owned,
                                    configure,
                                    &turn,
                                    res,
                                    binding,
                                )
                                .await
                            }
                            None => {
                                drive_foreground_pooled_chat(
                                    foreground_lease,
                                    pool,
                                    conv_owned,
                                    configure,
                                    &turn,
                                    res,
                                )
                                .await
                            }
                        }
                    };
                    let _delivered = terminal_tx.send(outcome);
                });
        if let Err(error) = supervision {
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
        Ok(aggregate_by_sentence(event_stream, channel_id, to, chat_type).await)
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
async fn parse_channel_mode_command(
    message: &str,
    mode: &tokio::sync::RwLock<echo_agent_app_core::tasks::task_runtime::InteractionMode>,
) -> Option<String> {
    use echo_agent_app_core::tasks::task_runtime::InteractionMode;

    let mut parts = message.split_whitespace();
    if parts.next()? != "/mode" {
        return None;
    }
    let Some(value) = parts.next() else {
        let current = mode.read().await;
        return Some(format!(
            "Current mode: {}. Usage: /mode chat|task|auto",
            current.as_str()
        ));
    };
    let next = match value.to_ascii_lowercase().as_str() {
        "chat" => InteractionMode::Chat,
        "task" => InteractionMode::Task,
        "auto" => InteractionMode::Auto,
        _ => return Some("Usage: /mode chat|task|auto".to_string()),
    };
    *mode.write().await = next;
    Some(format!("Interaction mode set to {}.", next.as_str()))
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
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    message: &str,
) -> Option<String> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/analysis" {
        return None;
    }
    let args: Vec<&str> = parts.collect();
    Some(crate::cli::cmd_impls::analysis::execute_analysis_command(agent, &args).await)
}

#[cfg(feature = "channels")]
async fn channel_papers_response(
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    message: &str,
) -> Option<String> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/papers" {
        return None;
    }
    let args: Vec<&str> = parts.collect();
    Some(crate::cli::cmd_impls::research::execute_papers_command(agent, &args).await)
}

#[cfg(feature = "channels")]
async fn channel_skills_response(
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    message: &str,
) -> Option<String> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/skills" {
        return None;
    }
    let args = parts.collect::<Vec<_>>();
    crate::cli::cmd_impls::skills::execute_skill_update_command(agent, &args).await
}

/// 将任意字符串清理为 DeepSeek `user_id` 合法形式 `[a-zA-Z0-9\-_]+`，最长 512 字符。
///
/// UTF-8 安全：用 `chars()` 迭代，禁止字节截断（中文/emoji → 替换为 `-`）。
/// 参考 AGENTS.md Rust 硬性约束 §1。
fn sanitize_cache_user_id(raw: &str) -> String {
    raw.chars()
        .take(512)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
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

#[cfg(feature = "channels")]
fn stage_channel_attachments(
    attachments: &[echo_agent::channels::MessageAttachment],
) -> Result<Vec<echo_agent_app_core::attachments::AttachmentRef>, String> {
    let mut staged = Vec::with_capacity(attachments.len());
    for (index, attachment) in attachments.iter().enumerate() {
        let data = channel_attachment_data(index, attachment);
        match echo_agent_app_core::attachments::stage_attachment_data(&data, None) {
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
const FLUSH_THRESHOLD: usize = 80;

/// 句末标点(中英文)触发 flush。
#[cfg(feature = "channels")]
fn is_sentence_end(c: char) -> bool {
    // 中文句末:。 ． ！ ？ … ;英文句末:. ! ?
    matches!(c, '。' | '．' | '！' | '？' | '…' | '.' | '!' | '?')
}

/// 把共享 `ChatDriverEvent` 流按句/段落聚合成逐段 `OutboundMessage` 流。
///
/// flush 条件(满足任一):
/// 1. buf 含换行 → flush 到最后一个换行(含),保留换行后的剩余。
/// 2. buf 以句末标点结尾 → flush 全 buf。
/// 3. buf.chars().count() >= FLUSH_THRESHOLD → flush 全 buf。
///
/// Agent terminal events flush buffered text; the application-owned terminal
/// receipt then renders an explicit cancelled/failed outcome and closes.
///
/// 生命周期:返回流借用 'a(随 `events`),由 `try_stream!` 自然处理(宏生成的
/// future 持有 `events` 的借用)。UTF-8 安全:全用 chars() 判长和拆分
/// (AGENTS.md §1);无 unwrap/expect(§2)。
#[cfg(feature = "channels")]
async fn aggregate_by_sentence<'a>(
    mut events: futures::stream::BoxStream<'a, echo_agent::error::Result<ChannelRenderEvent>>,
    channel_id: String,
    to: String,
    chat_type: echo_agent::channels::ChatType,
) -> futures::stream::BoxStream<'a, echo_agent::error::Result<echo_agent::channels::OutboundMessage>>
{
    use echo_agent::agent::AgentEvent;
    use echo_agent::channels::OutboundMessage;
    use echo_agent_app_core::chat_driver::ChatDriverEvent;
    use futures::StreamExt;

    let s = async_stream::try_stream! {
        let mut buf = String::new();
        // flush 全 buf(若非空)的统一动作,被多个终态/flush 分支共用。
        macro_rules! flush_all {
            () => {
                if !buf.is_empty() {
                    yield OutboundMessage::new(&channel_id, &to, chat_type, &buf);
                    buf.clear();
                }
            };
        }
        while let Some(ev) = events.next().await {
            match ev? {
                ChannelRenderEvent::Prompt(prompt) => {
                    flush_all!();
                    yield OutboundMessage::new(&channel_id, &to, chat_type, &prompt);
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::Agent(envelope)) => match envelope.payload {
                AgentEvent::Token(t) => {
                    buf.push_str(&t);
                    // 1. 换行 flush(到最后一个 \n 含)。反向字符偏移表示换行后
                    //    还有多少字符,因此 `cut` 是包含换行的字符数。
                    if let Some(trailing_chars) = buf.chars().rev().position(|ch| ch == '\n') {
                        let cut = buf.chars().count().saturating_sub(trailing_chars);
                        let chunk: String = buf.chars().take(cut).collect();
                        buf = buf.chars().skip(cut).collect();
                        yield OutboundMessage::new(&channel_id, &to, chat_type, &chunk);
                    }
                    // 2/3. 句末标点 或 阈值(chars().count() 非字节)→ flush 全 buf
                    else if buf.chars().last().map(is_sentence_end).unwrap_or(false)
                        || buf.chars().count() >= FLUSH_THRESHOLD
                    {
                        flush_all!();
                    }
                }
                AgentEvent::FinalAnswer(_) => {
                    flush_all!();
                }
                AgentEvent::Cancelled => {
                    flush_all!();
                }
                AgentEvent::Error { .. } => {
                    flush_all!();
                }
                AgentEvent::BudgetDecision { decision, reason, .. } => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[budget] {decision:?}: {reason}"),
                    );
                }
                AgentEvent::GuardTriggered { guard, blocked } => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[guard] {guard} (blocked={blocked})"),
                    );
                }
                AgentEvent::MemoryRecalled { count } => {
                    tracing::debug!(count, "channel agent recalled memory");
                }
                AgentEvent::Chart { spec } => {
                    flush_all!();
                    let preview: String = spec.to_string().chars().take(500).collect();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[chart] {preview}"),
                    );
                }
                AgentEvent::SafetyNotice { action, reason, risk, permission } => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[safety] {action}: {reason} (risk={risk}, permission={permission})"),
                    );
                }
                AgentEvent::ParameterError { tool, parameter, expected, got } => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[parameter] {tool}.{parameter}: expected {expected}, got {got}"),
                    );
                }
                _ => {}
                },
                ChannelRenderEvent::Driver(ChatDriverEvent::Execution(event)) => {
                    if event.event.is_attention_event() {
                        flush_all!();
                        let detail: String = event.payload.to_string().chars().take(500).collect();
                        yield OutboundMessage::new(
                            &channel_id,
                            &to,
                            chat_type,
                            format!("[task:{}] {}: {detail}", event.run_id, event.event),
                        );
                    }
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::TurnStatus { .. })
                | ChannelRenderEvent::Driver(ChatDriverEvent::ExecutionPath { .. })
                | ChannelRenderEvent::Driver(ChatDriverEvent::TurnConfiguration { .. }) => {}
                ChannelRenderEvent::Driver(ChatDriverEvent::Interrupt { run_id, goal, new_message }) => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[paused:{run_id}] {goal}; new instruction: {new_message}"),
                    );
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::InputQueued { input_id, .. }) => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[queued:{input_id}]"),
                    );
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::InputRemoved { .. }) => {}
                ChannelRenderEvent::Driver(ChatDriverEvent::InputReordered { .. }) => {}
                ChannelRenderEvent::Driver(ChatDriverEvent::CommandCellStarted { cell }) => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[cell:{}] started: {}", cell.cell_id, cell.name),
                    );
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::CommandCellSettled { cell }) => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[cell:{}] settled: {}", cell.cell_id, cell.phase),
                    );
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::AwaiterResultReady { result }) => {
                    flush_all!();
                    let event = ChatDriverEvent::AwaiterResultReady { result };
                    let message = echo_agent_app_core::tasks::task_runtime::project_awaiter_surface_event(&event)
                        .map(|projection| projection.display_message())
                        .unwrap_or_else(|| "Awaiter result is unavailable".to_string());
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        message,
                    );
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::AwaiterResultAcknowledged { .. }) => {}
                ChannelRenderEvent::Driver(ChatDriverEvent::ApprovalRequest {
                    request_id,
                    tool_name,
                    prompt,
                    ..
                }) => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[approval:{request_id}] {tool_name}: {prompt}"),
                    );
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::InputRequest { request_id, prompt }) => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[input:{request_id}] {prompt}"),
                    );
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::SelectionRequest {
                    request_id,
                    prompt,
                    options,
                    ..
                }) => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[selection:{request_id}] {prompt} ({})", options.join(", ")),
                    );
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::ContextCompressed {
                    before_count,
                    after_count,
                    before_tokens,
                    after_tokens,
                }) => {
                    flush_all!();
                    let saved = before_tokens.saturating_sub(after_tokens);
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!(
                            "[context] compressed {before_count}->{after_count} messages, \
                             {before_tokens}->{after_tokens} tokens ({saved} saved)"
                        ),
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
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        "[cancelled] The channel turn was cancelled.",
                    );
                    break;
                }
                ChannelRenderEvent::Terminal(
                    echo_agent_app_core::chat_driver::TurnOutcome::Failed(failure),
                ) => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[failed:{}] {}", failure.code, failure.message),
                    );
                    break;
                }
            }
        }
    };
    s.boxed()
}

#[cfg(test)]
mod tests {
    use super::sanitize_cache_user_id;

    #[cfg(feature = "channels")]
    #[test]
    fn channel_budget_parser_accepts_positive_or_unbounded() {
        assert_eq!(super::parse_channel_budget("42", "token"), Ok(Some(42)));
        assert_eq!(super::parse_channel_budget("unbounded", "time"), Ok(None));
        assert!(super::parse_channel_budget("0", "time").is_err());
    }

    #[test]
    fn ascii_passthrough() {
        assert_eq!(
            sanitize_cache_user_id("im-qqbot-user_123"),
            "im-qqbot-user_123"
        );
    }

    #[test]
    fn chinese_replaced_with_dash() {
        // 输入 8 字符: i m - 飞 书 - 张 三
        // 字面 `-` 保留,4 个中文各替换为 `-` → im + 6 个 `-`
        assert_eq!(sanitize_cache_user_id("im-飞书-张三"), "im------");
    }

    #[test]
    fn emoji_and_specials_replaced() {
        assert_eq!(sanitize_cache_user_id("a@b.c🦀d"), "a-b-c-d");
    }

    #[test]
    fn truncated_to_512_chars() {
        let raw: String = "x".repeat(600);
        let out = sanitize_cache_user_id(&raw);
        assert_eq!(out.chars().count(), 512);
        assert!(out.chars().all(|c| c == 'x'));
    }

    #[test]
    fn empty_input_yields_empty() {
        assert_eq!(sanitize_cache_user_id(""), "");
    }

    #[test]
    fn conversation_id_format() {
        assert_eq!(
            super::AppChannelMessageHandler::conversation_id("qqbot", "user_123"),
            "channel:qqbot:user_123"
        );
    }

    #[test]
    fn cache_user_id_format() {
        assert_eq!(
            super::AppChannelMessageHandler::cache_user_id("qqbot", "user_123"),
            "im-qqbot-user_123"
        );
    }

    #[cfg(feature = "channels")]
    #[test]
    fn management_commands_are_classified_exactly() {
        assert!(super::is_agent_management_command("/trace run-1"));
        assert!(super::is_agent_management_command(" /skills list "));
        assert!(!super::is_agent_management_command("/stop"));
        assert!(!super::is_agent_management_command("/traceable"));
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

    #[cfg(all(feature = "channels", unix))]
    #[tokio::test]
    async fn terminal_stream_keeps_fast_output_from_pre_dispatch_subscription() -> Result<(), String>
    {
        use echo_agent::channels::{ChatType, InboundMessage};
        use futures::StreamExt;

        let terminal = echo_agent_app_core::terminal::TerminalService::new();
        let receiver = terminal.subscribe();
        terminal
            .create("channel-fast".to_string(), None, 24, 80)
            .await?;
        terminal
            .write("channel-fast", b"printf channel-fast-output; exit\r")
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
        let texts = tokio::time::timeout(std::time::Duration::from_secs(5), async {
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
        assert!(texts.iter().any(|text| text.contains("exited:")));
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[test]
    fn downstream_drop_cancels_same_token_without_releasing_registry()
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
        lease.settle(TurnOutcome::Cancelled);
        assert!(
            control
                .snapshot(ForegroundTurnSurface::Channel, "channel:test")
                .is_none()
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn terminal_drains_accepted_final_answer_before_publication() -> Result<(), String> {
        use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity};
        use echo_agent_app_core::chat_driver::{ChatDriverEvent, TurnOutcome};
        use futures::StreamExt;

        let (driver_tx, driver_rx) = tokio::sync::mpsc::unbounded_channel();
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
            .send(ChatDriverEvent::Agent(Box::new(final_answer)))
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

        let (driver_tx, driver_rx) = tokio::sync::mpsc::unbounded_channel();
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

    // ── channel attachment transport tests ──────────────────────────────
    #[cfg(feature = "channels")]
    mod multimodal {
        use super::super::channel_attachment_data;
        use echo_agent::channels::{AttachmentKind, MessageAttachment};

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
    }

    // ── aggregate_by_sentence 测试(需 channels feature)──────────────────────
    #[cfg(feature = "channels")]
    mod aggregate {
        use super::super::{ChannelRenderEvent, FLUSH_THRESHOLD, aggregate_by_sentence};
        use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity};
        use echo_agent::channels::{ChatType, OutboundMessage};
        use echo_agent::error::Result;
        use futures::stream::{BoxStream, StreamExt};
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
            assert_eq!(texts, vec!["ab\n".to_string(), "c".to_string()]);
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
            assert_eq!(texts, vec!["你好。".to_string(), "再见".to_string()]);
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
            assert_eq!(texts, vec!["第一句！".to_string(), "第二句？".to_string()]);
        }
    }
}
