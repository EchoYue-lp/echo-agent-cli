//! Stage 2 — shared chat driver (极简入口).
//!
//! `drive_chat` is the single, thin entry for a chat turn across TUI / CLI
//! channel / GUI: it wraps the user input into one `Message`, streams the
//! agent's ReAct reply through a per-mode `ChatSink`, and stops. It does NOT
//! pre-judge normal vs complex and does NOT create a TaskRuntime run — the
//! agent itself decides whether to spin up a background run by calling the
//! `create_complex_task` tool (Phase B3+). Run lifecycle events flow back
//! through `sink.on_run_status` / `on_worker_trace`, never through this
//! entry's return value.
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
    /// so the **main agent's** task_tools (`execute_plan` / `delegate_readonly`)
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
/// agent's reply through `sink`, and returns. No route pre-judgment, no
/// TaskRuntime run creation — the agent decides whether a complex run is
/// warranted and triggers it itself via the `create_complex_task` tool.
pub async fn drive_chat(
    agent: &AgentHandle,
    message: &str,
    multimodal: Option<&Message>,
    res: std::sync::Arc<crate::chat_resources::ChatResources>,
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
        let mut stream = guard
            .execute_stream_message_with_cancel(msg, cancel)
            .await
            .map_err(|e| e.to_string())?;
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
