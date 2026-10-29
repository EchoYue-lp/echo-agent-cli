//! Per-turn EKO policy projected at every model boundary.
//!
//! User-authored text stays untouched in conversation history. Dynamic product
//! policy is carried in a replaceable projection so it cannot become part of
//! the user request or TaskRun goal.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, Instant};

use echo_agent::compression::{ContextProjection, PreModelContextProjector, ProjectionContext};
use echo_agent::error::Result as AgentResult;
use echo_agent::llm::types::Message;
use futures::future::BoxFuture;

use crate::prepared_turn::InstructionAuthorship;
use crate::tasks::task_runtime::RunTurnOrigin;
use crate::tasks::task_runtime::compact_context::{
    TaskRuntimeContextProjector, TaskRuntimeProjectionRegistry,
};

/// Stable projection owner for the current turn's product contract.
pub const TURN_CONTRACT_MARKER: &str = "eko:turn-contract";

const PROGRESS_REPORT_INTERVAL: Duration = Duration::from_secs(60);
const PROGRESS_REMINDER_MARKER: &str = "eko:progress-reminder";
const PROGRESS_REPORT_RULES: &str = "Progress communication: Give a brief user-visible update before the first tool operation and when a key finding, plan change, blocker, or verification result changes. Keep assistant progress separate from provider reasoning and tool output. Tool logs and runtime status are observations, not new findings. During a long tool or model call, do not start another model loop or interrupt the operation just to report progress.";

struct TurnProgressState {
    last_report: Instant,
    reminded_iteration: Option<usize>,
}

impl TurnProgressState {
    fn new(now: Instant) -> Self {
        Self {
            last_report: now,
            reminded_iteration: None,
        }
    }

    fn reminder_needed(&mut self, iteration: usize, now: Instant) -> bool {
        if now.saturating_duration_since(self.last_report) < PROGRESS_REPORT_INTERVAL {
            return false;
        }
        // 同一模型安全点可能因压缩而重新投影，复用一个替换式提醒；
        // 下一次请求不重复提醒，直到真正公开的自然语言报告重置计时。
        let reminded = self.reminded_iteration.get_or_insert(iteration);
        *reminded == iteration
    }

    fn reported(&mut self, now: Instant) {
        self.last_report = now;
        self.reminded_iteration = None;
    }
}

const LANGUAGE_PRIORITY_RULES: &str = r#"Response language priority:
1. An explicit output-language request wins.
2. Otherwise use the current user-authored request's natural language.
3. If that request has no clear language signal, use the most recent clear user-authored language available in conversation context, then the stable run goal or task brief.
Do not infer response language from system instructions, mode contracts, tool schemas, source code, terminal output, logs, plan metadata, dependency summaries, or previous assistant text. Preserve code, identifiers, paths, commands, protocol fields, exact required headings, and verbatim evidence."#;

#[derive(Clone, Copy)]
struct TurnContract {
    origin: RunTurnOrigin,
    authorship: InstructionAuthorship,
}

struct TurnRegistration {
    id: uuid::Uuid,
    contract: Option<TurnContract>,
    /// Already-read values belonging to this exact turn, never an I/O cache
    /// for another turn or a replacement for the Workspace location owner.
    project: Option<Arc<crate::infra::PreparedProjectContext>>,
    progress: TurnProgressState,
}

/// Process-stable bridge from interactive turn ownership into model calls.
pub struct TurnPromptContextRegistry {
    registrations: RwLock<HashMap<String, TurnRegistration>>,
}

impl TurnPromptContextRegistry {
    pub fn new() -> Self {
        Self {
            registrations: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(
        self: &Arc<Self>,
        turn_id: impl Into<String>,
        origin: RunTurnOrigin,
        authorship: InstructionAuthorship,
    ) -> TurnPromptContextRegistration {
        self.register_values(
            turn_id.into(),
            Some(TurnContract { origin, authorship }),
            None,
        )
    }

    pub(crate) fn register_project_context(
        self: &Arc<Self>,
        turn_id: String,
        prepared: crate::infra::PreparedProjectContext,
    ) -> TurnPromptContextRegistration {
        self.register_values(turn_id, None, Some(Arc::new(prepared)))
    }

    fn register_values(
        self: &Arc<Self>,
        turn_id: String,
        contract: Option<TurnContract>,
        project: Option<Arc<crate::infra::PreparedProjectContext>>,
    ) -> TurnPromptContextRegistration {
        let id = uuid::Uuid::new_v4();
        self.registrations
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                turn_id.clone(),
                TurnRegistration {
                    id,
                    contract,
                    project,
                    progress: TurnProgressState::new(Instant::now()),
                },
            );
        TurnPromptContextRegistration {
            registry: Arc::clone(self),
            turn_id,
            id,
        }
    }

    fn contract(&self, turn_id: &str) -> Option<TurnContract> {
        self.registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(turn_id)
            .and_then(|registration| registration.contract)
    }

    fn project_context(&self, turn_id: &str) -> Option<Arc<crate::infra::PreparedProjectContext>> {
        self.registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(turn_id)
            .and_then(|registration| registration.project.clone())
    }

    fn progress_reminder(&self, turn_id: &str, iteration: usize) -> bool {
        self.registrations
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .get_mut(turn_id)
            .is_some_and(|registration| {
                registration.contract.is_some()
                    && registration
                        .progress
                        .reminder_needed(iteration, Instant::now())
            })
    }

    pub fn contains(&self, turn_id: &str) -> bool {
        self.registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(turn_id)
    }
}

impl Default for TurnPromptContextRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TurnPromptContextRegistration {
    registry: Arc<TurnPromptContextRegistry>,
    turn_id: String,
    id: uuid::Uuid,
}

impl TurnPromptContextRegistration {
    pub(crate) fn progress_observer(&self) -> TurnProgressObserver {
        TurnProgressObserver {
            registry: Arc::clone(&self.registry),
            turn_id: self.turn_id.clone(),
            id: self.id,
        }
    }
    /// Publish only after the owned invocation's strict preparation succeeds.
    /// The registration id fences an old guard from replacing a newer turn.
    pub(crate) fn install_project_context(
        &self,
        prepared: crate::infra::PreparedProjectContext,
    ) -> Result<(), String> {
        let mut registrations = self
            .registry
            .registrations
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(registration) = registrations.get_mut(&self.turn_id)
            && registration.id == self.id
        {
            registration.project = Some(Arc::new(prepared));
            return Ok(());
        }
        Err("project turn registration changed before model admission".to_string())
    }
}

/// 只观察已向用户发布的报告；旧执行的回调不能重置新注册的时钟。
pub(crate) struct TurnProgressObserver {
    registry: Arc<TurnPromptContextRegistry>,
    turn_id: String,
    id: uuid::Uuid,
}

impl TurnProgressObserver {
    pub(crate) fn reported(&self) {
        let mut registrations = self
            .registry
            .registrations
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(registration) = registrations.get_mut(&self.turn_id)
            && registration.id == self.id
        {
            registration.progress.reported(Instant::now());
        }
    }
}

impl Drop for TurnPromptContextRegistration {
    fn drop(&mut self) {
        let mut registrations = self
            .registry
            .registrations
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if registrations
            .get(&self.turn_id)
            .is_some_and(|registration| registration.id == self.id)
        {
            registrations.remove(&self.turn_id);
        }
    }
}

static TURN_PROMPT_CONTEXT_REGISTRY: LazyLock<Arc<TurnPromptContextRegistry>> =
    LazyLock::new(|| Arc::new(TurnPromptContextRegistry::new()));

pub fn turn_prompt_context_registry() -> Arc<TurnPromptContextRegistry> {
    Arc::clone(&TURN_PROMPT_CONTEXT_REGISTRY)
}

/// EKO's application projector: TaskRuntime recovery plus active-turn policy.
pub struct EkoContextProjector {
    task_runtime: TaskRuntimeContextProjector,
    turns: Arc<TurnPromptContextRegistry>,
    command_cell_watch: Option<(
        Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>,
        crate::workspace::WorkspaceExecutionScope,
    )>,
    hot_memory: Arc<crate::unified_memory::HotMemoryProjectionSource>,
}

impl EkoContextProjector {
    pub fn new(
        task_runtime_registry: Arc<TaskRuntimeProjectionRegistry>,
        turns: Arc<TurnPromptContextRegistry>,
    ) -> Self {
        Self {
            task_runtime: TaskRuntimeContextProjector::new(task_runtime_registry),
            turns,
            command_cell_watch: None,
            hot_memory: Arc::new(crate::unified_memory::HotMemoryProjectionSource::new()),
        }
    }

    pub(crate) fn with_hot_memory_source(
        mut self,
        source: Arc<crate::unified_memory::HotMemoryProjectionSource>,
    ) -> Self {
        self.hot_memory = source;
        self
    }

    pub fn with_command_cell_watches(
        mut self,
        service: Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>,
        execution_scope: crate::workspace::WorkspaceExecutionScope,
    ) -> Self {
        self.command_cell_watch = Some((service, execution_scope));
        self
    }
}

impl PreModelContextProjector for EkoContextProjector {
    fn validate_model_request<'a>(
        &'a self,
        context: &'a ProjectionContext,
        request: &'a echo_agent::llm::ChatRequest,
        artifacts: Option<&'a echo_agent::tools::artifact::ToolOutputArtifactConfig>,
    ) -> BoxFuture<'a, AgentResult<()>> {
        self.task_runtime
            .validate_model_request(context, request, artifacts)
    }

    fn is_required(&self) -> bool {
        self.task_runtime.is_required()
    }

    fn project<'a>(
        &'a self,
        context: &'a ProjectionContext,
    ) -> BoxFuture<'a, AgentResult<Vec<ContextProjection>>> {
        Box::pin(async move {
            let mut projections = self.task_runtime.project(context).await?;
            if let Some(prepared) = context
                .turn_id
                .as_deref()
                .and_then(|turn_id| self.turns.project_context(turn_id))
            {
                // Execute/reset and checkpoint restore can replace messages.
                // Republish the same immutable values at the original model
                // boundary; no fallible file read occurs in this projector.
                projections.extend(prepared.projections());
            }
            let contract = context
                .turn_id
                .as_deref()
                .and_then(|turn_id| self.turns.contract(turn_id));
            projections.push(ContextProjection {
                marker: TURN_CONTRACT_MARKER.to_string(),
                message: contract.map(render_turn_contract).map(Message::user),
            });
            let remind = context
                .turn_id
                .as_deref()
                .is_some_and(|turn_id| self.turns.progress_reminder(turn_id, context.iteration));
            projections.push(ContextProjection {
                marker: PROGRESS_REMINDER_MARKER.to_string(),
                message: remind.then(|| Message::user("No user-visible natural-language progress report has been published for at least 60 seconds. At this existing model safe point, briefly report the current finding, changed plan, blocker, or verification result before continuing. If there is no new finding, say what you are still waiting for; do not invent progress or a completion.".to_string())),
            });
            projections.push(self.hot_memory.projection());
            let pending_command_cell_watch = match (
                self.command_cell_watch.as_ref(),
                context.conversation_id.as_deref(),
                context.turn_id.as_deref(),
            ) {
                (Some((service, scope)), Some(conversation_id), Some(turn_id)) => match service
                    .project_pending_command_cell_watches(
                        scope.workspace_id(),
                        conversation_id,
                        turn_id,
                    )
                    .await
                {
                    Ok(message) => message,
                    Err(error) => {
                        // The framework already treats this observer as an
                        // optional warning. Its failure must not discard the
                        // same batch's strictly prepared project rules or
                        // fixed Domain method after Execute/reset.
                        tracing::warn!(%error, "Pending command-cell watch projection failed");
                        None
                    }
                },
                _ => None,
            };
            projections.push(ContextProjection {
                marker: "[eko_pending_command_cell_watches]".to_string(),
                message: pending_command_cell_watch.map(Message::user),
            });
            Ok(projections)
        })
    }
}

fn render_turn_contract(contract: TurnContract) -> String {
    let language_anchor = match contract.authorship {
        InstructionAuthorship::User => {
            "This is a user-authored turn. Treat the non-projection user message as the current language anchor."
        }
        InstructionAuthorship::Runtime => {
            "This is a runtime-authored continuation turn. Treat the stable TaskRun goal and the most recent clear user-authored request as the language anchors, not the continuation instruction."
        }
    };
    format!(
        "[eko_turn_contract]\nTurn origin: {}\nInstruction author: {}\n\nResponse language:\n{language_anchor}\n{LANGUAGE_PRIORITY_RULES}\n\n{PROGRESS_REPORT_RULES}\n[/eko_turn_contract]",
        contract.origin.as_str(),
        contract.authorship.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn published_unknown_report_resets_original_sink_progress_without_counting_observations()
    -> anyhow::Result<()> {
        use crate::chat_driver::{ChatDriverEvent, ChatSink};
        use echo_agent::agent::{Agent, AgentEvent, AssistantMessageBlock, AssistantPhase};
        use echo_agent::guard::{Guard, GuardDirection, GuardResult};
        use echo_agent::llm::types::DeltaMessage;
        use echo_agent::testing::{MockLlmClient, MockTool, StreamChunk};
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Default)]
        struct ObservedProgress {
            aged: Option<Instant>,
            registration: Option<uuid::Uuid>,
            actual_turn: Option<String>,
            pending: Option<String>,
            effects: Vec<(String, bool)>,
            blocks: Vec<AssistantMessageBlock>,
            order: Vec<String>,
            tool_results: usize,
            error: Option<String>,
        }
        struct RealProgressSink {
            registry: Arc<TurnPromptContextRegistry>,
            observed: Arc<Mutex<ObservedProgress>>,
        }
        impl ChatSink for RealProgressSink {
            fn on_event(&self, event: ChatDriverEvent) -> bool {
                let ChatDriverEvent::Agent(envelope) = event else {
                    return true;
                };
                let result = (|| -> anyhow::Result<()> {
                    let mut observed = self.observed.lock().unwrap_or_else(|e| e.into_inner());
                    let mut entries = self
                        .registry
                        .registrations
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    let actual_turn = envelope.turn_id.as_str();
                    let registration = entries
                        .get_mut(actual_turn)
                        .ok_or_else(|| anyhow::anyhow!("original turn registration missing"))?;
                    let aged = match observed.aged {
                        Some(aged) => aged,
                        None => {
                            if !matches!(&envelope.payload, AgentEvent::ExecutionStatus(status)
                                if status.status == echo_agent::agent::ExecutionStatusKind::Started)
                            {
                                anyhow::bail!(
                                    "original Started must establish the progress observation"
                                );
                            }
                            let aged = Instant::now()
                                .checked_sub(Duration::from_secs(61))
                                .ok_or_else(|| anyhow::anyhow!("fixture instant cannot be aged"))?;
                            observed.aged = Some(aged);
                            observed.registration = Some(registration.id);
                            observed.actual_turn = Some(actual_turn.to_string());
                            aged
                        }
                    };
                    if observed.registration != Some(registration.id) {
                        anyhow::bail!("progress observer moved to another registration");
                    }
                    if observed.actual_turn.as_deref() != Some(actual_turn) {
                        anyhow::bail!("progress event crossed its original turn");
                    }
                    // reported()在真实sink接受事件之后运行；下一回调才观察上一事件的效果。
                    if let Some(previous) = observed.pending.take() {
                        observed
                            .effects
                            .push((previous, registration.progress.last_report != aged));
                    }
                    let label = match &envelope.payload {
                        AgentEvent::AssistantMessage { block } => {
                            observed.blocks.push(block.clone());
                            format!(
                                "{}:{:?}",
                                if block.delta.trim().is_empty() {
                                    "empty_block"
                                } else {
                                    "block"
                                },
                                block.phase
                            )
                        }
                        AgentEvent::ToolCall { .. } => "tool_call".to_string(),
                        AgentEvent::ToolResult { result, .. } => {
                            if result.success && result.output == "real-read-complete" {
                                observed.tool_results = observed.tool_results.saturating_add(1);
                            }
                            "tool_result".to_string()
                        }
                        AgentEvent::ExecutionStatus(_) => "status".to_string(),
                        AgentEvent::FinalAnswer(_) => "confirmed_final".to_string(),
                        AgentEvent::Error { .. } => "error".to_string(),
                        _ => "observation".to_string(),
                    };
                    observed.order.push(label.clone());
                    // 仅测试夹具把原有计时设回同一个过去值，逐事件隔离观测；不增加clock或等待。
                    registration.progress.last_report = aged;
                    observed.pending = Some(label);
                    Ok(())
                })();
                if let Err(error) = result {
                    self.observed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .error = Some(error.to_string());
                    return false;
                }
                true
            }
        }
        struct RejectOriginalFinal(Arc<AtomicUsize>);
        impl Guard for RejectOriginalFinal {
            fn name(&self) -> &str {
                "progress-whole-output-guard"
            }
            fn check<'a>(
                &'a self,
                content: &'a str,
                direction: GuardDirection,
            ) -> BoxFuture<'a, AgentResult<GuardResult>> {
                Box::pin(async move {
                    Ok(
                        if direction == GuardDirection::Output
                            && content.contains("SECRET_PROGRESS_FINAL")
                        {
                            self.0.fetch_add(1, Ordering::SeqCst);
                            GuardResult::Block {
                                reason: "original final withheld".to_string(),
                            }
                        } else {
                            GuardResult::Pass
                        },
                    )
                })
            }
        }

        for phase in [
            AssistantPhase::Unknown,
            AssistantPhase::Reasoning,
            AssistantPhase::FinalAnswer,
        ] {
            let guarded = phase == AssistantPhase::FinalAnswer;
            let turn_id = format!("published-progress-{phase:?}");
            let registry = turn_prompt_context_registry();
            let observed = Arc::new(Mutex::new(ObservedProgress::default()));
            let source = AssistantMessageBlock {
                provider_source: None,
                block_id: "progress-source-block".to_string(),
                request_id: "progress-source-request".to_string(),
                sequence: 0,
                phase,
                delta: if guarded {
                    "SECRET_PROGRESS_FINAL"
                } else {
                    "原始可见正文🙂"
                }
                .to_string(),
                complete: true,
                provider: Some("progress-fixture".to_string()),
                reasoning: None,
            };
            let calls = if guarded {
                serde_json::Value::Null
            } else {
                serde_json::json!([{"index":0,"id":"progress-call","type":"function","function":{"name":"read_file","arguments":"{}"}}])
            };
            let delta: DeltaMessage = serde_json::from_value(serde_json::json!({
                "role":"assistant", "content":if phase == AssistantPhase::Reasoning { "" } else { source.delta.as_str() },
                "assistant_blocks":[source.clone()], "tool_calls":calls
            }))?;
            let model = Arc::new(
                MockLlmClient::new()
                    .with_stream_script(vec![
                        StreamChunk::Delta(delta),
                        StreamChunk::Terminal {
                            finish_reason: Some(
                                if guarded { "stop" } else { "tool_calls" }.to_string(),
                            ),
                            usage: None,
                        },
                    ])
                    .with_response("safe completion"),
            );
            let guard_calls = Arc::new(AtomicUsize::new(0));
            let mut builder = echo_agent::agent::ReactAgentBuilder::new()
                .model("progress-fixture")
                .llm_client(model.clone())
                .tool(Box::new(
                    MockTool::new("read_file").with_response("real-read-complete"),
                ));
            if guarded {
                builder = builder.guard(Arc::new(RejectOriginalFinal(guard_calls.clone())));
            }
            let agent = crate::agent_handle::AgentHandle::new(builder.build()?);
            let sink: Arc<dyn ChatSink> = Arc::new(RealProgressSink {
                registry: registry.clone(),
                observed: observed.clone(),
            });
            let resources = Arc::new(crate::chat_resources::ChatResources {
                execution_scope: crate::workspace::WorkspaceExecutionScope::workspace(
                    &crate::workspace::WorkspaceId::from_name("progress-fixture"),
                    ".",
                ),
                workspace_io_receipt: None,
                pool: None,
                store: None,
                sink,
                webhook_emitter: None,
                conv_id: Some(turn_id.clone()),
                root_message_id: turn_id.clone(),
                attachments: Vec::new(),
                cancel: echo_agent::agent::CancellationToken::new(),
                review_integration: None,
                memory_generation: None,
                human_loop_provider: None,
            });
            let turn = crate::prepared_turn::PreparedUserTurn {
                instruction: "verify original published progress".to_string(),
                resources: Vec::new(),
                authorship: InstructionAuthorship::User,
                agent_input: None,
            };
            let outcome = crate::chat_driver::drive_chat(&agent, &turn, resources).await?;
            agent.inner().read().await.close().await?;
            assert!(
                !registry.contains(&turn_id),
                "original registration must naturally retire"
            );
            let observed = observed.lock().unwrap_or_else(|e| e.into_inner());
            assert!(observed.error.is_none(), "{:?}", observed.error);
            assert!(
                observed
                    .actual_turn
                    .as_deref()
                    .is_some_and(|id| !registry.contains(id))
            );
            if guarded {
                assert!(matches!(
                    outcome,
                    echo_agent::runtime::TurnOutcome::Failed(_)
                ));
                assert_eq!(guard_calls.load(Ordering::SeqCst), 1);
                assert_eq!(
                    model.call_count(),
                    1,
                    "the guard rejection must not start another model loop"
                );
                assert!(
                    !observed
                        .blocks
                        .iter()
                        .any(|block| block.delta.contains("SECRET_PROGRESS_FINAL"))
                );
                assert!(
                    !observed.effects.iter().any(|(_, changed)| *changed),
                    "withheld final and observations are not reports"
                );
            } else {
                assert_eq!(outcome, echo_agent::runtime::TurnOutcome::Completed);
                assert_eq!(
                    model.call_count(),
                    2,
                    "only the original tool and final requests may run"
                );
                assert_eq!(
                    observed.tool_results, 1,
                    "the original tool must actually execute"
                );
                assert!(
                    observed
                        .blocks
                        .iter()
                        .any(|block| block.block_id == source.block_id
                            && block.request_id == source.request_id
                            && block.phase == phase
                            && block.delta == source.delta)
                );
                let first_block = observed
                    .order
                    .iter()
                    .position(|label| label == &format!("block:{phase:?}"));
                let first_tool = observed.order.iter().position(|label| label == "tool_call");
                assert!(
                    matches!((first_block, first_tool), (Some(block), Some(tool)) if block < tool)
                );
                let effects = observed
                    .effects
                    .iter()
                    .filter(|(label, _)| label == &format!("block:{phase:?}"))
                    .collect::<Vec<_>>();
                assert!(
                    !effects.is_empty(),
                    "original block was not followed through the sink"
                );
                for (_, changed) in effects {
                    assert_eq!(
                        *changed,
                        phase == AssistantPhase::Unknown,
                        "approved public Unknown must reset the original report clock; Reasoning must not"
                    );
                }
                assert!(
                    observed.effects.iter().any(|(label, _)| label == "status"),
                    "the original execution status must be observed through the sink"
                );
                assert!(
                    observed
                        .effects
                        .iter()
                        .filter(|(label, _)| label.starts_with("empty_block:")
                            || matches!(label.as_str(), "tool_call" | "tool_result" | "status"))
                        .all(|(_, changed)| !changed),
                    "tool/status observations cannot reset progress"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn progress_reminder_waits_for_safe_point_and_only_rearms_after_report() {
        let start = Instant::now();
        let mut progress = TurnProgressState::new(start);
        assert!(!progress.reminder_needed(1, start + Duration::from_secs(59)));
        assert!(progress.reminder_needed(2, start + Duration::from_secs(60)));
        assert!(progress.reminder_needed(2, start + Duration::from_secs(61)));
        assert!(!progress.reminder_needed(3, start + Duration::from_secs(120)));
        progress.reported(start + Duration::from_secs(125));
        assert!(!progress.reminder_needed(4, start + Duration::from_secs(184)));
        assert!(progress.reminder_needed(5, start + Duration::from_secs(185)));
    }

    #[test]
    fn old_progress_observer_cannot_reset_replaced_turn() -> anyhow::Result<()> {
        let registry = Arc::new(TurnPromptContextRegistry::new());
        let old = registry.register("turn", RunTurnOrigin::User, InstructionAuthorship::User);
        let observer = old.progress_observer();
        let current = registry.register("turn", RunTurnOrigin::User, InstructionAuthorship::User);
        let before = registry
            .registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get("turn")
            .map(|registration| registration.progress.last_report)
            .ok_or_else(|| anyhow::anyhow!("current registration missing"))?;
        observer.reported();
        let after = registry
            .registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get("turn")
            .map(|registration| registration.progress.last_report);
        assert_eq!(after, Some(before));
        drop(old);
        assert!(registry.contains("turn"));
        drop(current);
        assert!(!registry.contains("turn"));
        Ok(())
    }

    #[derive(Default)]
    struct VerificationEvents(std::sync::Mutex<Vec<echo_agent::agent::EventEnvelope>>);

    #[async_trait::async_trait]
    impl echo_agent::runtime::EventSink for VerificationEvents {
        async fn on_event(
            &self,
            event: echo_agent::agent::EventEnvelope,
        ) -> AgentResult<echo_agent::runtime::SinkControl> {
            self.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event);
            Ok(echo_agent::runtime::SinkControl::Continue)
        }
    }

    #[tokio::test]
    async fn parallel_verifications_share_root_and_run_without_sharing_project_projection()
    -> anyhow::Result<()> {
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, RunPlanPolicy, TaskRunExecutionProfile, TaskRuntimeStore,
        };
        use echo_agent::agent::{AgentInvocationContext, EventIdentity, ReactAgentBuilder};
        use echo_agent::runtime::{AgentTurnDriver, TurnMode, TurnOutcome, TurnRequest};
        let fixture =
            crate::tasks::task_runtime::project_environment_tests::EnvironmentFixture::new()
                .map_err(anyhow::Error::msg)?;
        // A real target repository bounds the existing VCS-first discovery;
        // local namesakes and the fixture's escape rejection stay unchanged.
        crate::tasks::task_runtime::worktree::run_git(
            &fixture.target_root,
            &["init", "-b", "main"],
        )?;
        let task_registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let turns = Arc::new(TurnPromptContextRegistry::new());
        let binding = crate::domain_plugin::test_binding(false).map_err(anyhow::Error::msg)?;
        let run_id = uuid::Uuid::new_v4().to_string();
        let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
        store.create_run_for_active_workspace_with_domain(
            &run_id,
            "parallel-conversation",
            "shared-root",
            DomainProfile::General,
            "verify independent targets",
            "task",
            AttendedMode::Attended,
            TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
            Some(&binding),
        )?;
        let _domain = task_registry
            .retain_for_invocation(&run_id, store)
            .map_err(anyhow::Error::msg)?;
        let events_root = fixture.data_root.join("watch-events");
        let event_log = Arc::new(crate::chat_event_log::ChatEventLog::open(
            &events_root,
            crate::chat_event_log::ChatEventRetention::default(),
        )?);
        let watches = crate::tasks::task_runtime::command_cells::CommandCellRuntimeService::new(
            fixture.executor.clone(),
            event_log,
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(anyhow::Error::msg)?;
        let events_backup = fixture.data_root.join("watch-events-backup");
        std::fs::rename(&events_root, &events_backup)?;
        std::fs::write(&events_root, "unavailable optional watch source")?;
        assert!(
            watches
                .project_pending_command_cell_watches(
                    "workspace",
                    "parallel-conversation",
                    "verification-a",
                )
                .await
                .is_err()
        );
        let mut cases = Vec::new();
        let mut registrations = Vec::new();
        for (execution, rule) in [
            ("verification-a", "PROJECT_A_RULE"),
            ("verification-b", "PROJECT_B_RULE"),
        ] {
            let cwd = fixture.visible_root.join(execution);
            fixture.files.create_dir_all(&cwd)?;
            fixture
                .files
                .write(&cwd.join("Cargo.toml"), b"fixture marker")?;
            fixture
                .files
                .write(&cwd.join("AGENTS.md"), rule.as_bytes())?;
            std::fs::create_dir_all(&cwd)?;
            std::fs::write(cwd.join("AGENTS.md"), "WRONG_LOCAL_PROJECT_RULE")?;
            fixture.files.write(
                &cwd.join("probe.txt"),
                format!("TARGET_FILE_{execution}").as_bytes(),
            )?;
            std::fs::write(cwd.join("probe.txt"), "WRONG_LOCAL_FILE")?;
            let client = Arc::new(
                echo_agent::testing::MockLlmClient::new()
                    .then_tool_call(
                        format!("read-{execution}"),
                        "read_file",
                        r#"{"path":"probe.txt"}"#,
                    )
                    .with_response("verified"),
            );
            let mut agent = ReactAgentBuilder::new()
                .name(execution)
                .model("verification-test")
                .system_prompt("PRIMARY_VERIFICATION_ROLE")
                .auto_project_rules(false)
                .file_system(fixture.files.clone())
                .working_dir(cwd.clone())
                .llm_client(client.clone())
                .tool(Box::new(
                    echo_agent::tools::files::files::ReadFileTool::new()
                        .with_file_system(fixture.files.clone()),
                ))
                .max_iterations(2)
                .build()?;
            agent.set_sandbox_executor(fixture.executor.clone());
            agent.set_pre_model_context_projector(Some(Arc::new(
                EkoContextProjector::new(task_registry.clone(), turns.clone())
                    .with_command_cell_watches(
                        watches.clone(),
                        crate::workspace::WorkspaceExecutionScope::workspace(
                            &crate::workspace::WorkspaceId::from_raw("workspace".to_string()),
                            &cwd,
                        ),
                    ),
            )));
            let prepared = crate::infra::prepare_dynamic_project_context(
                Some(&cwd),
                None,
                fixture.files.clone(),
                fixture.executor.clone(),
                Vec::new(),
                agent.config().get_token_limit(),
            )
            .await
            .map_err(anyhow::Error::msg)?;
            registrations.push(turns.register_project_context(execution.to_string(), prepared));
            let invocation = AgentInvocationContext {
                working_dir: Some(cwd),
                runtime: Some(echo_agent::tools::ExternalRunContext {
                    conversation_id: Some("parallel-conversation".to_string()),
                    run_id: Some(run_id.clone()),
                    turn_id: Some(execution.to_string()),
                    execution_id: Some(execution.to_string()),
                    message_id: Some("shared-root".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let identity = EventIdentity::from_invocation(&invocation)?;
            let request = TurnRequest::new(identity, "verify")
                .mode(TurnMode::Execute)
                .invocation(invocation);
            cases.push((
                agent,
                client,
                request,
                VerificationEvents::default(),
                execution,
                rule,
            ));
        }
        let first = cases
            .first()
            .ok_or_else(|| anyhow::anyhow!("first verification absent"))?;
        let second = cases
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("second verification absent"))?;
        // Both exact registrations exist before either model starts. No sleep
        // or scheduler ordering can hide one turn overwriting the other.
        let (a, b) = tokio::join!(
            AgentTurnDriver.drive(&first.0, first.2.clone(), &first.3),
            AgentTurnDriver.drive(&second.0, second.2.clone(), &second.3),
        );
        assert_eq!(a?.outcome, TurnOutcome::Completed);
        assert_eq!(b?.outcome, TurnOutcome::Completed);
        for (_, client, _, events, execution, rule) in &cases {
            let calls = client.all_calls();
            assert_eq!(calls.len(), 2);
            let text = calls
                .first()
                .ok_or_else(|| anyhow::anyhow!("verification model call absent"))?
                .iter()
                .filter_map(|message| message.content.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.matches(*rule).count(), 1);
            assert!(text.contains(&binding.snapshot.method));
            assert!(!text.contains("WRONG_LOCAL_PROJECT_RULE"));
            let foreign = if *execution == "verification-a" {
                "PROJECT_B_RULE"
            } else {
                "PROJECT_A_RULE"
            };
            assert!(!text.contains(foreign));
            let after_tool = calls
                .last()
                .ok_or_else(|| anyhow::anyhow!("tool result model call absent"))?
                .iter()
                .filter_map(|message| message.content.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(after_tool.contains(&format!("TARGET_FILE_{execution}")));
            assert!(!after_tool.contains("WRONG_LOCAL_FILE"));
            let foreign_file = if *execution == "verification-a" {
                "TARGET_FILE_verification-b"
            } else {
                "TARGET_FILE_verification-a"
            };
            assert!(!after_tool.contains(foreign_file));
            let events = events.0.lock().unwrap_or_else(|error| error.into_inner());
            assert!(!events.is_empty());
            assert!(
                events
                    .iter()
                    .all(|event| event.turn_id.as_str() == *execution
                        && event.execution_id.as_ref().map(|id| id.as_str()) == Some(*execution)
                        && event.message_id.as_ref().map(|id| id.as_str()) == Some("shared-root")
                        && event.run_id.as_ref().map(|id| id.as_str()) == Some(run_id.as_str()))
            );
        }
        let mut registrations = registrations.into_iter();
        drop(registrations.next());
        assert!(!turns.contains("verification-a"));
        assert!(turns.contains("verification-b"));
        drop(registrations);
        assert!(!turns.contains("verification-b"));
        std::fs::remove_file(&events_root)?;
        std::fs::rename(&events_backup, &events_root)?;
        // Restoring the fixture does not acknowledge its earlier recovery
        // failure. Let the original read/recovery owner settle that debt.
        assert!(
            watches
                .project_pending_command_cell_watches(
                    "workspace",
                    "parallel-conversation",
                    "verification-recovery",
                )
                .await
                .map_err(anyhow::Error::msg)?
                .is_none()
        );
        watches.shutdown().await.map_err(anyhow::Error::msg)?;
        Ok(())
    }

    fn projection_context(turn_id: Option<&str>) -> ProjectionContext {
        ProjectionContext {
            iteration: 3,
            agent_name: "eko".to_string(),
            session_id: None,
            conversation_id: Some("conversation".to_string()),
            run_id: None,
            turn_id: turn_id.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn turn_contract_is_scoped_and_removed_after_registration_drop() -> Result<(), String> {
        let task_registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let turn_registry = Arc::new(TurnPromptContextRegistry::new());
        let projector = EkoContextProjector::new(task_registry, Arc::clone(&turn_registry));
        let registration =
            turn_registry.register("turn-a", RunTurnOrigin::User, InstructionAuthorship::User);

        let active = projector
            .project(&projection_context(Some("turn-a")))
            .await
            .map_err(|error| error.to_string())?;
        let contract = active
            .iter()
            .find(|projection| projection.marker == TURN_CONTRACT_MARKER)
            .and_then(|projection| projection.message.as_ref())
            .and_then(|message| message.content.as_text())
            .ok_or_else(|| "active turn contract missing".to_string())?;
        if !contract.contains("Turn origin: user")
            || !contract.contains("non-projection user message")
        {
            return Err(format!("turn contract is incomplete: {contract}"));
        }

        drop(registration);
        let inactive = projector
            .project(&projection_context(Some("turn-a")))
            .await
            .map_err(|error| error.to_string())?;
        let stale = inactive
            .iter()
            .find(|projection| projection.marker == TURN_CONTRACT_MARKER)
            .and_then(|projection| projection.message.as_ref());
        if stale.is_some() || turn_registry.contains("turn-a") {
            return Err("turn contract leaked beyond its registration".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn continuation_uses_run_goal_as_language_anchor() -> Result<(), String> {
        let task_registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let turn_registry = Arc::new(TurnPromptContextRegistry::new());
        let projector = EkoContextProjector::new(task_registry, Arc::clone(&turn_registry));
        let _registration = turn_registry.register(
            "turn-c",
            RunTurnOrigin::Continuation,
            InstructionAuthorship::Runtime,
        );

        let projections = projector
            .project(&projection_context(Some("turn-c")))
            .await
            .map_err(|error| error.to_string())?;
        let contract = projections
            .iter()
            .find(|projection| projection.marker == TURN_CONTRACT_MARKER)
            .and_then(|projection| projection.message.as_ref())
            .and_then(|message| message.content.as_text())
            .ok_or_else(|| "continuation contract missing".to_string())?;
        if !contract.contains("Turn origin: continuation")
            || !contract.contains("stable TaskRun goal")
            || !contract.contains("not the continuation instruction")
        {
            return Err(format!(
                "continuation language anchor is incomplete: {contract}"
            ));
        }
        Ok(())
    }

    #[test]
    fn latest_registration_owns_cleanup_for_the_same_turn() -> Result<(), String> {
        let registry = Arc::new(TurnPromptContextRegistry::new());
        let old = registry.register("turn", RunTurnOrigin::User, InstructionAuthorship::User);
        let current = registry.register(
            "turn",
            RunTurnOrigin::Resume,
            InstructionAuthorship::Runtime,
        );
        drop(old);
        let contract = registry
            .contract("turn")
            .ok_or_else(|| "current registration missing".to_string())?;
        if contract.origin != RunTurnOrigin::Resume
            || contract.authorship != InstructionAuthorship::Runtime
        {
            return Err("old registration removed the current turn".to_string());
        }
        drop(current);
        if registry.contains("turn") {
            return Err("current registration did not clean up".to_string());
        }
        Ok(())
    }
}
