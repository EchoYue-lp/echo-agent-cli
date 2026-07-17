//! Stage 2 — shared chat driver (极简入口).
//!
//! `drive_chat` is the single, thin entry for a chat turn across TUI / CLI
//! channel / GUI: it wraps the user input into one `Message`, streams the
//! agent's ReAct reply through a per-mode `ChatSink`, and stops. It does not
//! classify Auto requests in advance. Task mode creates its required formal
//! run before execution; Auto creates a run only when the agent invokes a
//! formal plan or long-lived task tool; ordinary Chat/Auto turns create none.
//!
//! Multimodal is passed through (`Option<&Message>`) so TUI / channel can
//! attach images/files the same way GUI already does.

use echo_agent::agent::{
    Agent, AgentEvent, AgentHandle, EventEnvelope, EventIdentity, envelope_event_stream,
};
use echo_agent::prelude::Message;
use echo_core::tools::TraceSinkFn;
use futures::StreamExt;

use crate::tasks::task_runtime::executor::ExecEvent;

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

/// Per-mode event consumer for the shared chat driver.
///
/// Each mode provides one exhaustive product-event entry point:
/// - GUI (`TauriChatSink`, in the Tauri layer) emits Tauri events.
/// - TUI/CLI render the stream to the terminal.
/// - channel aggregates by sentence + forwarding.
pub trait ChatSink: Send + Sync + 'static {
    /// Return `false` to stop the current stream because the consumer closed.
    fn on_event(&self, event: ChatDriverEvent) -> bool;
}

/// Build the EKO TaskRuntime sink carried through task-local run context.
pub fn worker_trace_sink_for(
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
/// `plan_create` / `plan_execute` / `create_complex_task` 等依赖
/// `require_run_id()` 的工具时,能从 task_local 读到 run_id,不再被
/// `"no active run — run_id not set in context"` 提前拒绝(对齐 Claude Code
/// 的无门槛只读 dispatch)。
///
/// turn_id 进入 Agent ExternalRunContext；普通聊天不写 TaskRuntimeStore。
/// `create_complex_task` 和 inline/formal plan 各自拥有真正的 run_id。
pub async fn drive_chat(
    agent: &AgentHandle,
    message: &str,
    multimodal: Option<&Message>,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
) -> Result<(), String> {
    // Scope a per-turn run_id so task tools (plan_create /
    // plan_execute / create_complex_task) can read it via require_run_id().
    // Use root_message_id (unique per turn, set by all 3 callers); fall back to
    // a fresh uuid if a caller forgot to set it (defensive, never panics).
    let turn_id = if res.root_message_id.trim().is_empty() {
        tracing::warn!("drive_chat: root_message_id empty — using fallback uuid as turn_id");
        uuid::Uuid::new_v4().to_string()
    } else {
        res.root_message_id.clone()
    };

    let cancel = res.cancel.clone();
    let turn_cancel = cancel.clone();
    let sink = res.sink.clone();
    let trace_sink = worker_trace_sink_for(&sink);
    let formal_run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&turn_id);
    let interaction_mode = res.interaction_mode;
    let store = res.store.clone();
    if interaction_mode == crate::tasks::task_runtime::InteractionMode::Task {
        ensure_task_mode_run(
            store.as_ref(),
            &formal_run_id,
            res.conv_id.as_deref(),
            &turn_id,
            message,
            &res.attachments,
            Some(&trace_sink),
        )?;
    }
    let _cancel_registration =
        if interaction_mode == crate::tasks::task_runtime::InteractionMode::Task {
            match store.as_ref() {
                Some(store) => Some(
                    store
                        .register_run_cancellation(&formal_run_id, cancel.clone())
                        .map_err(|error| error.to_string())?,
                ),
                None => None,
            }
        } else {
            None
        };
    let turn_id_for_inner = turn_id.clone();
    let _projection_registration = res.store.as_ref().map(|store| {
        crate::tasks::task_runtime::compact_context::task_runtime_projection_registry()
            .register(formal_run_id.clone(), std::sync::Arc::clone(store))
    });
    let result = crate::tasks::task_runtime::task_tools::with_run_context(
        formal_run_id.clone(),
        cancel,
        Some(trace_sink.clone()),
        drive_chat_inner(agent, message, multimodal, res, turn_id_for_inner),
    )
    .await;
    if interaction_mode == crate::tasks::task_runtime::InteractionMode::Task {
        finalize_task_mode_run(
            store.as_ref(),
            &formal_run_id,
            turn_cancel.is_cancelled(),
            Some(&trace_sink),
        );
    }
    let requested_mode = interaction_mode.as_str();
    let observed_path =
        observe_execution_path(store.as_ref(), &formal_run_id, &turn_id, requested_mode);
    let _ = sink.on_event(ChatDriverEvent::ExecutionPath {
        requested_mode: requested_mode.to_string(),
        observed_path: observed_path.to_string(),
    });
    tracing::info!(
        requested_mode,
        observed_path,
        turn_id,
        "chat execution path observed"
    );
    result
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
        .create_run(
            run_id,
            "default",
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
                "run_started",
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

fn finalize_task_mode_run(
    store: Option<&std::sync::Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    run_id: &str,
    cancelled: bool,
    trace_sink: Option<&crate::tasks::task_runtime::task_tools::TraceSink>,
) {
    use crate::tasks::task_runtime::TaskRunStatus;

    let Some(store) = store else {
        return;
    };
    let Ok(Some(run)) = store.get_run(run_id) else {
        return;
    };
    if run.status != TaskRunStatus::Running {
        return;
    }
    if cancelled {
        if store
            .transition_run(run_id, TaskRunStatus::Cancelled)
            .is_ok()
            && let Some(trace_sink) = trace_sink
        {
            trace_sink(ExecEvent::run(
                run_id.to_string(),
                "run_cancelled",
                serde_json::json!({ "status": "cancelled", "mode": "task" }),
            ));
        }
        return;
    }
    let reason = match store.get_plan(run_id) {
        Ok(Some(_)) => "Task mode turn ended before plan_execute reached a terminal result",
        _ => "Task mode turn ended without creating a formal plan",
    };
    let _ = store.note(run_id, None, reason);
    if store.transition_run(run_id, TaskRunStatus::Failed).is_ok()
        && let Some(trace_sink) = trace_sink
    {
        trace_sink(ExecEvent::run(
            run_id.to_string(),
            "run_failed",
            serde_json::json!({ "error": reason, "mode": "task" }),
        ));
    }
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
    } else if matching.iter().any(|run| run.route == "agent_inline_task") {
        "inline_subagent"
    } else {
        "direct"
    };
    for run in matching {
        let _ = store.record_execution_path(&run.run_id, requested_mode, observed);
    }
    observed
}

/// Inner ReAct-streaming body of [`drive_chat`], run inside the run_id scope.
async fn drive_chat_inner(
    agent: &AgentHandle,
    message: &str,
    multimodal: Option<&Message>,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    turn_id: String,
) -> Result<(), String> {
    let msg: Message = match multimodal {
        Some(m) => m.clone(),
        None => {
            // B4.3 (spec §8): prepend the per-turn mode hint to the user text
            // when set (Chat/Task modes). Pure prompt — no code route branch,
            // no re-introduction of route pre-judgment. Auto (None) adds none.
            let text = match &res.mode_hint {
                Some(hint) if !hint.is_empty() => {
                    format!("[Mode: {hint}]\n\n{message}")
                }
                _ => message.to_string(),
            };
            Message::user(text)
        }
    };
    let cancel = res.cancel.clone();
    let sink: std::sync::Arc<dyn ChatSink> = res.sink.clone();
    // P1.1: capture interaction mode before `res` is moved into the chat
    // resources scope, so we can apply Chat-mode tool hiding after acquiring
    // the agent read guard.
    let interaction_mode = res.interaction_mode;
    let conversation_id = res.conv_id.clone();
    // Scope the chat resources into a task_local so tools the agent calls
    // mid-ReAct (create_complex_task / check_run_status / cancel_run, Phase B3)
    // can reach pool/store/sink via `current_chat_resources()`.
    crate::chat_resources::with_chat_resources(res, async move {
        // The RwLock read guard is held for the stream's lifetime because the
        // stream borrows the agent (same pattern as the GUI's normal chat path).
        let inner = agent.inner().clone();
        let guard = inner.read().await;
        // Chat 模式下物理隐藏任务管理工具(不只是 prompt hint)。工具排除属于
        // invocation 值,不会修改 pooled agent 的共享状态。
        use crate::tasks::task_runtime::InteractionMode;
        let disabled_tools = if interaction_mode == InteractionMode::Chat {
            Some(
                [
                    "plan_create",
                    "task_update",
                    "task_skip",
                    "task_list",
                    "plan_execute",
                    "create_complex_task",
                    "check_run_status",
                    "cancel_run",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            )
        } else {
            None
        };

        // `with_run_context` is task-local and does not cross the framework's
        // forked subagent `tokio::spawn`; ExternalRunContext is the value-carried
        // channel that keeps worker tools and run_id on this same run. The
        // `trace_sink` here is the framework-Value form; `scoped_with_ctx_run_id`
        // re-scopes it into `CURRENT_TRACE_SINK` for tools (e.g. plan_execute)
        // running inside the framework's spawned tool executor.
        let event_identity = EventIdentity {
            conversation_id: conversation_id.clone(),
            run_id: None,
            turn_id: turn_id.clone(),
            execution_id: None,
            parent_event_id: None,
        };
        let invocation = echo_core::agent::AgentInvocationContext {
            runtime: Some(echo_core::tools::ExternalRunContext {
                conversation_id,
                run_id: None,
                turn_id: Some(turn_id.clone()),
                execution_id: None,
                message_id: Some(turn_id),
                cancel: Some(std::sync::Arc::new(cancel.clone())),
                trace_sink: Some(framework_trace_sink_for(&sink)),
                delegation_policy: None,
            }),
            working_dir: None,
            cancel: None,
            disabled_tools,
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
                let _ = sink.on_event(ChatDriverEvent::Agent(Box::new(EventEnvelope::new(
                    &event_identity,
                    1,
                    None,
                    AgentEvent::Error {
                        source: "chat_driver".into(),
                        message: e.to_string(),
                    },
                ))));
                return Err(e.to_string());
            }
        };
        let mut stream = envelope_event_stream(raw_stream, event_identity);
        async {
            while let Some(event_result) = stream.next().await {
                match event_result {
                    Ok(event) => {
                        if !sink.on_event(ChatDriverEvent::Agent(Box::new(event))) {
                            break;
                        }
                    }
                    Err(e) => {
                        // The envelope adapter normalizes raw stream errors into
                        // terminal payloads. This branch remains for future
                        // transport adapters that can fail independently.
                        tracing::warn!(error = %e, "agent stream error during chat");
                        break;
                    }
                }
            }
            Ok::<(), String>(())
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

    struct CountingTool {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl echo_core::tools::Tool for CountingTool {
        fn name(&self) -> &str {
            "create_complex_task"
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
        fn event_count(&self) -> usize {
            self.events.lock().unwrap_or_else(|e| e.into_inner()).len()
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
                    && event.conversation_id.as_deref() == Some(conversation_id)
                    && event.turn_id == turn_id
                    && event.run_id.is_none()
                    && !event.event_id.is_empty()
            }) && terminal_count == 1
        }

        fn execution_paths(&self) -> Vec<(String, String)> {
            self.execution_paths
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        }
    }

    #[tokio::test]
    async fn task_mode_creates_formal_run_and_rejects_direct_fallback() -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;

        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("t")
                .with_response("direct answer without plan"),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(llm)
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
            conv_id: Some("task-conversation".to_string()),
            root_message_id: "task-turn".to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            mode_hint: Some(
                crate::tasks::task_runtime::InteractionMode::Task
                    .prompt_hint()
                    .to_string(),
            ),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Task,
            layer_manager: None,
        });

        drive_chat(&agent, "build a formal plan", None, resources).await?;

        let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn("task-turn");
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "formal task run missing".to_string())?;
        assert_eq!(
            run.status,
            crate::tasks::task_runtime::TaskRunStatus::Failed
        );
        assert_eq!(run.route, "agent_task_plan");
        assert_eq!(
            chat_sink.execution_paths(),
            vec![("task".to_string(), "formal_plan".to_string())]
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
            conv_id: Some("c1".to_string()),
            root_message_id: "m1".to_string(),
            attachments: vec![],
            cancel,
            mode_hint: None,
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Auto,
            layer_manager: None,
        });
        drive_chat(&agent, "hi", None, res)
            .await
            .expect("drive_chat should succeed");
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
    }

    #[tokio::test]
    async fn drive_chat_projection_survives_snapshot_spawn_and_unregisters() -> Result<(), String> {
        use crate::tasks::task_runtime::compact_context::{
            RUNTIME_RECOVERY_MARKER, TaskRuntimeContextProjector, task_runtime_projection_registry,
        };
        use crate::tasks::task_runtime::types::{
            AttendedMode, DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, TaskPlan,
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
        react_agent.set_pre_model_context_projector(Some(Arc::new(
            TaskRuntimeContextProjector::new(task_runtime_projection_registry()),
        )));
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
            .attach_plan(&TaskPlan {
                plan_id: "boundary-plan".to_string(),
                run_id: run_id.clone(),
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
            conv_id: Some("c1".to_string()),
            root_message_id: turn_id.to_string(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            mode_hint: None,
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Auto,
            layer_manager: None,
        });

        drive_chat(&agent, "continue", None, res).await?;

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
        {
            return Err("runtime projection did not cross snapshot/spawn boundary".to_string());
        }
        if task_runtime_projection_registry().contains(&run_id) {
            return Err("drive_chat did not unregister projection on exit".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn drive_chat_prepends_mode_hint_to_user_text() {
        // B4.3 (spec §8): the per-turn mode_hint is prepended to the user text
        // as a bracketed note (pure prompt, no code route branch). Verify the
        // LLM actually receives the prefixed message.
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
                .llm_client(mock.clone())
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
            store: Some(store),
            sink,
            conv_id: None,
            root_message_id: "m1".to_string(),
            attachments: vec![],
            cancel,
            mode_hint: Some("Chat — do not spawn tasks".to_string()),
            interaction_mode: crate::tasks::task_runtime::InteractionMode::Chat,
            layer_manager: None,
        });
        drive_chat(&agent, "hi there", None, res)
            .await
            .expect("drive_chat should succeed");
        let messages = mock
            .last_messages()
            .expect("LLM should have been called at least once");
        let user_text = messages
            .iter()
            .filter_map(|m| {
                use echo_core::llm::types::Role;
                if m.role == Role::User {
                    m.content.as_text()
                } else {
                    None
                }
            })
            .next()
            .expect("at least one user message should be sent");
        assert!(
            user_text.contains("[Mode: Chat — do not spawn tasks]"),
            "user text should carry the mode hint prefix; got: {user_text}"
        );
        assert!(
            user_text.contains("hi there"),
            "original user text should follow the hint; got: {user_text}"
        );
    }

    #[tokio::test]
    async fn chat_tool_exclusions_are_invocation_scoped_on_pooled_agent() -> Result<(), String> {
        use echo_agent::agent::CancellationToken;
        use std::sync::Arc;

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mock = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("t")
                .then_tool_call("chat-call", "create_complex_task", "{}")
                .with_response("chat done")
                .then_tool_call("auto-call", "create_complex_task", "{}")
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
                conv_id: None,
                root_message_id: root_message_id.to_string(),
                attachments: Vec::new(),
                cancel: CancellationToken::new(),
                mode_hint: None,
                interaction_mode,
                layer_manager: None,
            });
            drive_chat(&agent, "run", None, resources).await?;
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
        let identity = EventIdentity {
            turn_id: "turn-1".to_string(),
            ..EventIdentity::default()
        };

        // on_event forwards each event to the channel and keeps going.
        assert!(
            sink.on_event(ChatDriverEvent::Agent(Box::new(EventEnvelope::new(
                &identity,
                1,
                None,
                AgentEvent::Token("hel".to_string()),
            )))),
            "on_event should return true to continue"
        );
        assert!(
            sink.on_event(ChatDriverEvent::Agent(Box::new(EventEnvelope::new(
                &identity,
                2,
                None,
                AgentEvent::Token("lo".to_string()),
            )))),
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
