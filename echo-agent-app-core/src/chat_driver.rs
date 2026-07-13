//! Stage 2 — shared chat driver (极简入口).
//!
//! `drive_chat` is the single, thin entry for a chat turn across TUI / CLI
//! channel / GUI: it wraps the user input into one `Message`, streams the
//! agent's ReAct reply through a per-mode `ChatSink`, and stops. It does NOT
//! pre-judge normal vs complex. The per-turn TaskRuntime run is created only
//! to give task tools and forked subagents one canonical run/trace context;
//! the agent still decides whether to call `plan_create`, `plan_execute`, or
//! `create_complex_task` (Phase B3+).
//!
//! Multimodal is passed through (`Option<&Message>`) so TUI / channel can
//! attach images/files the same way GUI already does.

use echo_agent::agent::{
    Agent, AgentEvent, AgentHandle, EventEnvelope, EventIdentity, envelope_event_stream,
};
use echo_agent::prelude::Message;
use echo_core::tools::TraceSinkFn;
use futures::StreamExt;

/// Per-mode event consumer for the shared chat driver.
///
/// Default methods are no-op; each mode overrides what it needs:
/// - GUI (`TauriChatSink`, in the Tauri layer) emits Tauri events.
/// - TUI/CLI render the stream to the terminal.
/// - channel aggregates by sentence + forwarding.
///
/// `on_agent_event` is the only required method — it consumes one event from
/// the chat/run stream and returns `false` to stop early (e.g. on cancel).
pub trait ChatSink: Send + Sync + 'static {
    fn on_agent_event(&self, event: EventEnvelope) -> bool;
    fn on_run_status(&self, _status: &str) {}
    fn on_interrupt(&self, _run_id: &str, _goal: &str, _new_message: &str) {}
    /// Trace sink forwarded into the framework's external run context
    /// (`ExternalRunContext.trace_sink`) so tools running inside a spawned
    /// task executor (e.g. `plan_execute`) can still reach
    /// `CURRENT_TRACE_SINK` via `scoped_with_ctx_run_id`. The framework
    /// carries `serde_json::Value` (not the app's `ExecEvent`) to stay
    /// decoupled; the app re-deserializes on the way back out. GUI provides a
    /// Tauri-emitting closure, non-GUI modes return `None`.
    fn trace_sink(&self) -> Option<TraceSinkFn> {
        None
    }
    /// Trace sink scoped into the task_local run context (`with_run_context`)
    /// so the **main agent's** task_tools (`plan_execute`) can emit
    /// [`crate::tasks::task_runtime::executor::ExecEvent`]s during a complex run.
    /// GUI provides a closure that rewrites the main-agent run_id + emits to
    /// the frontend's unified `execution://event` channel; non-GUI modes
    /// return `None` (trace events dropped, functionality unaffected).
    fn worker_trace_sink(&self) -> Option<crate::tasks::task_runtime::task_tools::TraceSink> {
        None
    }
}

/// Drive a chat turn through the single shared path (极简入口).
///
/// Wraps `message` (plus optional `multimodal`) into one `Message`, streams the
/// agent's reply through `sink`, and returns. No route pre-judgment; the
/// turn id is only the shared context anchor for task tools and forked
/// subagents. A TaskRuntime run is created lazily only when the agent actually
/// creates or executes a plan. The agent still decides whether a complex run is
/// warranted by calling `plan_create`, `plan_execute`, or
/// `create_complex_task`.
///
/// ## turn/run identity
///
/// 普通 chat 轮次使用 `res.root_message_id` 作 turn_id。task tools 若被调用，
/// 会从该 turn_id 派生独立的 `taskrun:<turn_id>`，并按需创建正式 TaskRun。这样
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
    let trace_sink = res.sink.worker_trace_sink();
    let formal_run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&turn_id);
    let turn_id_for_inner = turn_id.clone();
    let _projection_registration = res.store.as_ref().map(|store| {
        crate::tasks::task_runtime::compact_context::task_runtime_projection_registry()
            .register(formal_run_id.clone(), std::sync::Arc::clone(store))
    });
    crate::tasks::task_runtime::task_tools::with_run_context(
        formal_run_id,
        cancel,
        trace_sink,
        drive_chat_inner(agent, message, multimodal, res, turn_id_for_inner),
    )
    .await
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
                    "task_complete",
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
                trace_sink: sink.trace_sink(),
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
                let _ = sink.on_agent_event(EventEnvelope::new(
                    &event_identity,
                    1,
                    None,
                    AgentEvent::Error {
                        source: "chat_driver".into(),
                        message: e.to_string(),
                    },
                ));
                return Err(e.to_string());
            }
        };
        let mut stream = envelope_event_stream(raw_stream, event_identity);
        async {
            while let Some(event_result) = stream.next().await {
                match event_result {
                    Ok(event) => {
                        if !sink.on_agent_event(event) {
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

/// A `ChatSink` that forwards every `EventEnvelope` to an mpsc channel.
///
/// Used by modes whose renderer consumes a stream of event envelopes over a
/// channel — TUI (forwards to the UI render loop) and IM channels (aggregate
/// by sentence). Other event kinds (run status / worker trace / interrupt)
/// are no-op for these modes: they render the stream directly and don't need
/// GUI-style side-event emission.
pub struct ChannelChatSink {
    tx: tokio::sync::mpsc::UnboundedSender<EventEnvelope>,
}

impl ChannelChatSink {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<EventEnvelope>) -> Self {
        Self { tx }
    }
}

impl ChatSink for ChannelChatSink {
    fn on_agent_event(&self, event: EventEnvelope) -> bool {
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
    }

    impl Default for MockChatSink {
        fn default() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl ChatSink for MockChatSink {
        fn on_agent_event(&self, event: EventEnvelope) -> bool {
            self.events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(event);
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
    async fn channel_chat_sink_forwards_events() {
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::unbounded_channel::<EventEnvelope>();
        let sink = ChannelChatSink::new(tx);
        let identity = EventIdentity {
            turn_id: "turn-1".to_string(),
            ..EventIdentity::default()
        };

        // on_agent_event forwards each event to the channel and keeps going.
        assert!(
            sink.on_agent_event(EventEnvelope::new(
                &identity,
                1,
                None,
                AgentEvent::Token("hel".to_string()),
            )),
            "on_agent_event should return true to continue"
        );
        assert!(
            sink.on_agent_event(EventEnvelope::new(
                &identity,
                2,
                None,
                AgentEvent::Token("lo".to_string()),
            )),
            "second event should also be accepted"
        );

        let first = rx.recv().await.expect("first event should be forwarded");
        let second = rx.recv().await.expect("second event should be forwarded");
        match first.payload {
            AgentEvent::Token(t) => assert_eq!(t, "hel"),
            other => panic!("first should be Token(\"hel\"); got {other:?}"),
        }
        match second.payload {
            AgentEvent::Token(t) => assert_eq!(t, "lo"),
            other => panic!("second should be Token(\"lo\"); got {other:?}"),
        }
    }
}
