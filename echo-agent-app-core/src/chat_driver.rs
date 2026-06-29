//! Stage 2 — shared chat driver.
//!
//! Unifies TUI/CLI/channel/GUI chat through a single app-core entry
//! (`drive_chat`) that routes normal vs complex and streams `AgentEvent`s
//! through a per-mode `ChatSink`. This eliminates A3 (the three non-GUI entry
//! points bypassed routing by calling `chat_stream` directly) and lets each
//! mode render the shared stream its own way.
//!
//! See `docs/stage2-chat-driver-unification-spec.md`.

use std::sync::Arc;

use echo_agent::agent::{Agent, AgentEvent, AgentHandle, CancellationToken};
use echo_agent::prelude::Message;
use echo_core::tools::TraceSinkFn;
use futures::StreamExt;

use crate::tasks::task_runtime::router::TaskRouteDecision;
use crate::tasks::task_runtime::store::TaskRuntimeStore;
use crate::tasks::task_runtime::types::{AttendedMode, TaskRunStatus, WorkerTraceEvent};

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
    fn on_route_decision(&self, _decision: &TaskRouteDecision) {}
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

/// Outcome of driving a chat turn.
pub struct ChatOutcome {
    /// `Some(run_id)` when a TaskRuntime run was created (complex route);
    /// `None` for normal streaming chat.
    pub run_id: Option<String>,
}

/// Drive a chat turn through the shared path.
///
/// For a normal route, stream the agent's reply through `sink` without
/// creating a run. For a complex route, create a TaskRuntime run, transition
/// it to Running, and launch the unified run driver via `launch_unified_run_core`.
pub async fn drive_chat(
    agent: &AgentHandle,
    message: &str,
    decision: &TaskRouteDecision,
    sink: &dyn ChatSink,
    cancel: CancellationToken,
    store: Option<&TaskRuntimeStore>,
    conv_id: Option<&str>,
) -> Result<ChatOutcome, String> {
    if !decision.route.should_create_runtime_run() {
        // Normal chat: stream the agent's reply through the sink without
        // creating a run. The RwLock read guard is held for the stream's
        // lifetime because the stream borrows the agent (same pattern as the
        // GUI's normal chat path in `tauri/commands/chat.rs`).
        let inner = agent.inner().clone();
        let guard = inner.read().await;
        let mut stream = guard
            .execute_stream_with_cancel(message, cancel)
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
                    tracing::warn!(error = %e, "agent stream error during normal chat");
                    break;
                }
            }
        }
        return Ok(ChatOutcome { run_id: None });
    }

    // Complex route: create a run, transition to Running, launch the driver.
    let store = store.ok_or_else(|| {
        "complex route requires a TaskRuntime store, but none was provided".to_string()
    })?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let conv = conv_id
        .map(str::to_string)
        .unwrap_or_else(|| format!("message:{run_id}"));
    store
        .create_run(
            &run_id,
            "default",
            &conv,
            "",
            decision.classification.inferred_profile,
            message,
            decision.route.as_str(),
            AttendedMode::Attended,
        )
        .map_err(|e| e.to_string())?;
    store
        .transition_run(&run_id, TaskRunStatus::Running)
        .map_err(|e| e.to_string())?;

    sink.on_route_decision(decision);
    let terminal = launch_unified_run_core(agent, &run_id, message, None, sink, cancel).await?;

    // Transition the run to its terminal status. Mirror the GUI's guard: only
    // transition if the run is not already terminal (execute_plan's inner
    // execute_run may have set a more accurate status first).
    let new_status = match terminal.as_str() {
        "completed" => TaskRunStatus::Completed,
        "cancelled" => TaskRunStatus::Cancelled,
        _ => TaskRunStatus::Failed,
    };
    let already_terminal = store
        .get_run(&run_id)
        .map_err(|e| e.to_string())?
        .map(|r| {
            matches!(
                r.status,
                TaskRunStatus::Completed | TaskRunStatus::Failed | TaskRunStatus::Cancelled
            )
        })
        .unwrap_or(false);
    if !already_terminal && let Err(e) = store.transition_run(&run_id, new_status) {
        tracing::error!(error = %e, run_id = %run_id, "terminal transition failed");
    }

    Ok(ChatOutcome {
        run_id: Some(run_id),
    })
}

/// Drive an already-created TaskRuntime run to completion through `sink`.
///
/// This is the Tauri-free core extracted from the GUI's
/// `launch_unified_run` (`tauri/commands/chat.rs`): it injects the run context
/// (so workers inherit `run_id`/`cancel`/`trace_sink`), streams the agent's
/// reply through `sink.on_agent_event`, and signals run lifecycle
/// (`running` → `completed`/`cancelled`/`failed`) via `sink.on_run_status`.
/// Run creation + attachment setup stay with the caller.
pub async fn launch_unified_run_core(
    agent: &AgentHandle,
    run_id: &str,
    goal: &str,
    multimodal: Option<&Message>,
    sink: &dyn ChatSink,
    cancel: CancellationToken,
) -> Result<String, String> {
    use crate::tasks::task_runtime::task_tools::with_run_context;
    use echo_core::tools::ExternalRunContext;

    sink.on_run_status("running");

    let worker_trace_sink = sink.worker_trace_sink();
    let goal_owned = goal.to_string();
    let run_id_owned = run_id.to_string();

    // Scope the task_local run context around the whole driver so the main
    // agent's task_tools (`execute_plan` / `delegate_readonly`) can read
    // run_id / cancel / trace_sink. Without this, a complex run where the main
    // agent calls execute_plan would fail ("no active run" in task_local).
    // (GUI's `launch_unified_run` does the same via `with_run_context`.)
    let terminal = with_run_context(
        run_id_owned.clone(),
        cancel.clone(),
        worker_trace_sink,
        async {
            // The RwLock read guard is held for the run's lifetime (the stream
            // borrows the agent) — same pattern as the GUI's launch_unified_run.
            let inner = agent.inner().clone();
            let guard = inner.read().await;

            // Inject the run context so workers (delegate_readonly / execute_plan)
            // inherit run_id / cancel / trace_sink via `build_runtime_context`,
            // bypassing the task_local that would break across spawns.
            guard.set_external_context(&ExternalRunContext {
                run_id: run_id_owned.clone(),
                cancel: Some(Arc::new(cancel.clone())),
                trace_sink: sink.trace_sink(),
            });

            let mut terminal = "completed".to_string();
            let stream_result = if let Some(msg) = multimodal {
                guard
                    .execute_stream_message_with_cancel(msg.clone(), cancel.clone())
                    .await
            } else {
                guard
                    .execute_stream_with_cancel(&goal_owned, cancel.clone())
                    .await
            };
            if let Ok(mut stream) = stream_result {
                while let Some(event_result) = stream.next().await {
                    if cancel.is_cancelled() {
                        terminal = "cancelled".to_string();
                        break;
                    }
                    match event_result {
                        Ok(event) => {
                            if !sink.on_agent_event(event) {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "agent stream error during run");
                            terminal = "failed".to_string();
                            break;
                        }
                    }
                }
            } else if let Err(e) = stream_result {
                tracing::warn!(error = %e, "agent stream setup error during run");
                terminal = "failed".to_string();
            }
            terminal
        },
    )
    .await;

    sink.on_run_status(&terminal);
    Ok(terminal)
}

/// A `ChatSink` that forwards every `AgentEvent` to an mpsc channel.
///
/// Used by modes whose renderer consumes a stream of `AgentEvent`s over a
/// channel — TUI (forwards to the UI render loop) and IM channels (aggregate
/// by sentence). Other event kinds (run status / worker trace / route
/// decision / interrupt) are no-op for these modes: they render the stream
/// directly and don't need GUI-style side-event emission.
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
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Test-only sink that records received events for assertions.
    struct MockChatSink {
        events: std::sync::Mutex<Vec<AgentEvent>>,
        statuses: std::sync::Mutex<Vec<String>>,
        route_called: AtomicBool,
    }

    impl Default for MockChatSink {
        fn default() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
                statuses: std::sync::Mutex::new(Vec::new()),
                route_called: AtomicBool::new(false),
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
        fn on_run_status(&self, status: &str) {
            self.statuses
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(status.to_string());
        }
        fn on_route_decision(&self, _decision: &TaskRouteDecision) {
            self.route_called.store(true, Ordering::Relaxed);
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
        fn statuses(&self) -> Vec<String> {
            self.statuses
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
        fn route_decision_called(&self) -> bool {
            self.route_called.load(Ordering::Relaxed)
        }
    }

    #[tokio::test]
    async fn drive_chat_normal_streams_agent_events_via_sink() {
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
        let sink = MockChatSink::default();
        let decision = TaskRouteDecision::normal("test");
        let outcome = drive_chat(&agent, "hi", &decision, &sink, cancel, None, None)
            .await
            .expect("drive_chat normal should succeed");
        // Normal chat streams the agent's FinalAnswer through the sink.
        assert!(
            sink.has_final_answer(),
            "normal chat should stream FinalAnswer to sink; events recorded: {}",
            sink.event_count()
        );
        assert!(
            outcome.run_id.is_none(),
            "normal chat must not create a run"
        );
        assert!(
            !sink.route_decision_called(),
            "normal chat must not emit a route decision"
        );
    }

    #[tokio::test]
    async fn launch_unified_run_core_drives_agent_and_signals_sink() {
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
        let sink = MockChatSink::default();
        launch_unified_run_core(&agent, "run-1", "do it", None, &sink, cancel)
            .await
            .expect("launch_unified_run_core should succeed");
        // The core drives the agent to FinalAnswer + signals the lifecycle.
        assert!(
            sink.has_final_answer(),
            "core should stream FinalAnswer; events recorded: {}",
            sink.event_count()
        );
        let statuses = sink.statuses();
        assert!(
            statuses.iter().any(|s| s == "running"),
            "should signal running; got {:?}",
            statuses
        );
        assert!(
            statuses.iter().any(|s| s == "completed"),
            "should signal completed; got {:?}",
            statuses
        );
    }

    #[tokio::test]
    async fn drive_chat_complex_creates_run_and_launches() {
        use crate::tasks::task_runtime::router::route_message_with_feedback;
        use crate::tasks::task_runtime::store::TaskRuntimeStore;
        use crate::tasks::task_runtime::types::InteractionMode;
        use std::sync::Arc;

        // Task mode forces a complex route (no LLM needed).
        let decision =
            route_message_with_feedback(None, "build a todo app", InteractionMode::Task, &[]).await;
        assert!(
            decision.route.should_create_runtime_run(),
            "Task mode should yield a complex route; got {:?}",
            decision.route
        );

        let mock = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("t")
                .with_response("done"),
        );
        let agent = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .build()
                .expect("test agent should build"),
        );
        let store = TaskRuntimeStore::new_in_memory().expect("in-memory store should build");
        let cancel = CancellationToken::new();
        let sink = MockChatSink::default();

        let outcome = drive_chat(
            &agent,
            "build a todo app",
            &decision,
            &sink,
            cancel,
            Some(&store),
            Some("conv-1"),
        )
        .await
        .expect("drive_chat complex should succeed");

        // Complex route creates a run and returns its id.
        let run_id = outcome
            .run_id
            .as_ref()
            .expect("complex route should return a run_id");
        let run = store
            .get_run(run_id)
            .expect("store read should succeed")
            .expect("run should exist in store");
        assert!(
            matches!(
                run.status,
                crate::tasks::task_runtime::types::TaskRunStatus::Completed
            ),
            "run should be Completed after launch; got {:?}",
            run.status
        );
        // The driver streamed through the sink + signalled the route decision.
        assert!(
            sink.route_decision_called(),
            "complex route should emit the route decision"
        );
        assert!(
            sink.has_final_answer(),
            "complex route should stream FinalAnswer; got {} events",
            sink.event_count()
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
