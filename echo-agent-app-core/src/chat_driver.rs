//! Stage 2 — shared chat driver (极简入口).
//!
//! `drive_chat` is the single, thin entry for a chat turn across TUI / CLI
//! channel / GUI: it wraps the user input into one `Message`, streams the
//! agent's ReAct reply through a per-mode `ChatSink`, and stops. It does NOT
//! pre-judge normal vs complex. The per-turn TaskRuntime run is created only
//! to give task tools and forked subagents one canonical run/trace context;
//! the agent still decides whether to call `task_create`, `execute_plan`, or
//! `create_complex_task` (Phase B3+).
//!
//! Multimodal is passed through (`Option<&Message>`) so TUI / channel can
//! attach images/files the same way GUI already does.

use echo_agent::agent::{Agent, AgentEvent, AgentHandle};
use echo_agent::prelude::Message;
use echo_core::tools::TraceSinkFn;
use futures::StreamExt;

use crate::tasks::task_runtime::types::WorkerTraceEvent;

/// Per-mode event consumer for the shared chat driver.
///
/// Default methods are no-op; each mode overrides what it needs:
/// - GUI (`TauriChatSink`, in the Tauri layer) emits Tauri events.
/// - TUI/CLI render the stream to the terminal.
/// - channel aggregates by sentence + forwards.
///
/// `on_agent_event` is the only required method — it consumes one event from
/// the chat/run stream and returns `false` to stop early (e.g. on cancel).
pub trait ChatSink: Send + Sync + 'static {
    fn on_agent_event(&self, event: AgentEvent) -> bool;
    fn on_run_status(&self, _status: &str) {}
    fn on_worker_trace(&self, _event: WorkerTraceEvent) {}
    fn on_interrupt(&self, _run_id: &str, _goal: &str, _new_message: &str) {}
    /// Trace sink forwarded into the framework's external run context
    /// (`ExternalRunContext.trace_sink`) so worker trace events reach the
    /// frontend during a complex run. The framework consumes `serde_json::Value`
    /// (not the app-layer event type) to stay decoupled; GUI provides a
    /// Tauri-emitting closure, non-GUI modes return `None`.
    fn trace_sink(&self) -> Option<TraceSinkFn> {
        None
    }
    /// Trace sink scoped into the task_local run context (`with_run_context`)
    /// so the **main agent's** task_tools (`execute_plan`)
    /// can emit `WorkerTraceEvent`s during a complex run. GUI provides a closure
    /// that rewrites the main-agent run_id + emits to the frontend; non-GUI modes
    /// return `None` (trace events dropped, functionality unaffected).
    fn worker_trace_sink(&self) -> Option<crate::tasks::task_runtime::task_tools::TraceSink> {
        None
    }
}

/// Drive a chat turn through the single shared path (极简入口).
///
/// Wraps `message` (plus optional `multimodal`) into one `Message`, streams the
/// agent's reply through `sink`, and returns. No route pre-judgment; the
/// per-turn TaskRuntime run is only the shared context anchor for task tools
/// and forked subagents. The agent still decides whether a complex run is
/// warranted by calling `task_create`, `execute_plan`, or
/// `create_complex_task`.
///
/// ## run_id scoping (防"真空区"死结)
///
/// 普通 chat 轮次也包一层 `with_run_context`,用 `res.root_message_id` 作
/// run_id。这样主 agent 在 ReAct 循环里调
/// `task_create` / `execute_plan` / `create_complex_task` 等依赖
/// `require_run_id()` 的工具时,能从 task_local 读到 run_id,不再被
/// `"no active run — run_id not set in context"` 提前拒绝(对齐 Claude Code
/// 的无门槛只读 dispatch)。
///
/// 该 run_id 同时写入 TaskRuntimeStore 和 Agent ExternalRunContext,作为
/// 本轮 task/subagent trace 的统一锚点。`create_complex_task` 仍会 new 一个
/// Uuid 作背景 run id,不与这个前台 chat run 冲突。
pub async fn drive_chat(
    agent: &AgentHandle,
    message: &str,
    multimodal: Option<&Message>,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
) -> Result<(), String> {
    // Scope a per-turn run_id so task tools (task_create /
    // execute_plan / create_complex_task) can read it via require_run_id().
    // Use root_message_id (unique per turn, set by all 3 callers); fall back to
    // a fresh uuid if a caller forgot to set it (defensive, never panics).
    let run_id = if res.root_message_id.trim().is_empty() {
        tracing::warn!("drive_chat: root_message_id empty — using fallback uuid as run_id");
        uuid::Uuid::new_v4().to_string()
    } else {
        res.root_message_id.clone()
    };

    // Ensure the TaskRuntimeStore has a run record for this turn's run_id.
    // drive_chat scopes run_id=root_message_id into task_local so task_* tools
    // can read it, but without a create_run the store has no record → every
    // task_create write becomes an orphan (no RunCreated event ancestor) and
    // rebuild_plan_from_events discards them → task_list returns empty.
    // Idempotent: skips if the run already exists (create_complex_task / a
    // resumed turn may have created it).
    if let Some(store) = res.store.as_ref() {
        let already_exists = store.get_run(&run_id).ok().flatten().is_some();
        if !already_exists {
            let conv = res
                .conv_id
                .clone()
                .unwrap_or_else(|| format!("message:{run_id}"));
            if let Err(e) = store.create_run(
                &run_id,
                "default",
                &conv,
                &run_id,
                crate::tasks::task_runtime::types::DomainProfile::General,
                message,
                "chat_turn",
                crate::tasks::task_runtime::types::AttendedMode::Attended,
            ) {
                tracing::warn!(
                    error = %e,
                    run_id = %run_id,
                    "drive_chat: ad-hoc create_run failed (task tools may not persist)"
                );
            } else {
                let _ = store.transition_run(
                    &run_id,
                    crate::tasks::task_runtime::types::TaskRunStatus::Running,
                );
            }
        }
    }

    let cancel = res.cancel.clone();
    let trace_sink = res.sink.worker_trace_sink();
    let run_id_for_inner = run_id.clone();
    crate::tasks::task_runtime::task_tools::with_run_context(
        run_id,
        cancel,
        trace_sink,
        drive_chat_inner(agent, message, multimodal, res, run_id_for_inner),
    )
    .await
}

/// Inner ReAct-streaming body of [`drive_chat`], run inside the run_id scope.
async fn drive_chat_inner(
    agent: &AgentHandle,
    message: &str,
    multimodal: Option<&Message>,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
    run_id: String,
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
    // Scope the chat resources into a task_local so tools the agent calls
    // mid-ReAct (create_complex_task / check_run_status / cancel_run, Phase B3)
    // can reach pool/store/sink via `current_chat_resources()`.
    crate::chat_resources::with_chat_resources(res, async move {
        // The RwLock read guard is held for the stream's lifetime because the
        // stream borrows the agent (same pattern as the GUI's normal chat path).
        let inner = agent.inner().clone();
        let guard = inner.read().await;

        // `with_run_context` is task-local and does not cross the framework's
        // forked subagent `tokio::spawn`; ExternalRunContext is the value-carried
        // channel that keeps worker tools and trace events on this same run.
        guard.set_external_context(&echo_core::tools::ExternalRunContext {
            run_id: run_id.clone(),
            execution_id: None,
            // Chat path: run_id == root_message_id (set in drive_chat), so the
            // subagent stream can be pinned to this turn's message block.
            message_id: Some(run_id),
            cancel: Some(std::sync::Arc::new(cancel.clone())),
            trace_sink: sink.trace_sink(),
        });

        let stream_result = guard.execute_stream_message_with_cancel(msg, cancel).await;
        let mut stream = match stream_result {
            Ok(stream) => stream,
            Err(e) => {
                guard.clear_external_context();
                return Err(e.to_string());
            }
        };
        let result = async {
            while let Some(event_result) = stream.next().await {
                match event_result {
                    Ok(event) => {
                        if !sink.on_agent_event(event) {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "agent stream error during chat");
                        break;
                    }
                }
            }
            Ok::<(), String>(())
        }
        .await;
        guard.clear_external_context();
        result
    })
    .await
}

/// A `ChatSink` that forwards every `AgentEvent` to an mpsc channel.
///
/// Used by modes whose renderer consumes a stream of `AgentEvent`s over a
/// channel — TUI (forwards to the UI render loop) and IM channels (aggregate
/// by sentence). Other event kinds (run status / worker trace / interrupt)
/// are no-op for these modes: they render the stream directly and don't need
/// GUI-style side-event emission.
pub struct ChannelChatSink {
    tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
}

impl ChannelChatSink {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>) -> Self {
        Self { tx }
    }
}

impl ChatSink for ChannelChatSink {
    fn on_agent_event(&self, event: AgentEvent) -> bool {
        // Forward to the channel; if the receiver was dropped (UI quit /
        // channel closed), return false so the driver stops streaming.
        self.tx.send(event).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only sink that records received events for assertions.
    struct MockChatSink {
        events: std::sync::Mutex<Vec<AgentEvent>>,
    }

    impl Default for MockChatSink {
        fn default() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl ChatSink for MockChatSink {
        fn on_agent_event(&self, event: AgentEvent) -> bool {
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
                .any(|e| matches!(e, AgentEvent::FinalAnswer(_)))
        }
        fn event_count(&self) -> usize {
            self.events.lock().unwrap_or_else(|e| e.into_inner()).len()
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
            store: Some(store),
            sink,
            conv_id: None,
            root_message_id: "m1".to_string(),
            attachments: vec![],
            cancel,
            mode_hint: None,
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
    async fn channel_chat_sink_forwards_events() {
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        let sink = ChannelChatSink::new(tx);

        // on_agent_event forwards each event to the channel and keeps going.
        assert!(
            sink.on_agent_event(AgentEvent::Token("hel".to_string())),
            "on_agent_event should return true to continue"
        );
        assert!(
            sink.on_agent_event(AgentEvent::Token("lo".to_string())),
            "second event should also be accepted"
        );

        let first = rx.recv().await.expect("first event should be forwarded");
        let second = rx.recv().await.expect("second event should be forwarded");
        match first {
            AgentEvent::Token(t) => assert_eq!(t, "hel"),
            other => panic!("first should be Token(\"hel\"); got {other:?}"),
        }
        match second {
            AgentEvent::Token(t) => assert_eq!(t, "lo"),
            other => panic!("second should be Token(\"lo\"); got {other:?}"),
        }
    }
}
