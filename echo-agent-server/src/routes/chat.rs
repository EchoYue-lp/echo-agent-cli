//! 对话 API

use axum::{
    Json, debug_handler,
    extract::State,
    response::sse::{Event as SseEvent, Sse},
};
use echo_agent::agent::Agent;
use futures::{StreamExt, stream};
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;

use crate::error::WebError;
use crate::state::AppState;
use crate::types::{ChatRequest, ChatResponse, ContextStats, ToolCallInfo};

const MAX_MESSAGE_LENGTH: usize = 32768;

/// POST /api/chat - 阻塞式对话
///
/// Spawns stream processing into a background task. Note: the agent read lock
/// is still held for the full duration of stream consumption because
/// `chat_stream()` returns a stream that borrows `&self`. The spawned task
/// isolates the lock from the HTTP handler, but does NOT reduce the lock
/// duration.
///
/// True lock reduction requires either:
/// - `chat_stream()` returning an owned stream (internal state snapshot), or
/// - per-session agent instances so each session has its own lock.
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn handle_chat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, WebError> {
    // Input validation
    let msg = req.message.trim();
    if msg.is_empty() {
        return Err(WebError::Validation("Message cannot be empty".to_string()));
    }
    if msg.len() > MAX_MESSAGE_LENGTH {
        return Err(WebError::Validation(format!(
            "Message too long: {} bytes (max {})",
            msg.len(),
            MAX_MESSAGE_LENGTH
        )));
    }

    let agent_arc = state.connection.agent.inner().clone();
    let message = msg.to_string();

    // session_id context restore is not yet supported on the shared agent.
    // See handle_chat_stream for details.
    if req.session_id.is_some() {
        tracing::warn!("session_id is not yet supported on the shared agent; ignoring");
    }

    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let agent = agent_arc.read().await;

        let max_iterations = agent.config().get_max_iterations();
        let mut final_answer = String::new();
        let mut tool_calls = Vec::new();

        let stream_result = agent.chat_stream(&message).await;
        let result = match stream_result {
            Ok(mut stream) => {
                let mut current_tool: Option<(String, Value)> = None;

                while let Some(event_result) = stream.next().await {
                    match event_result {
                        Ok(event) => match event {
                            echo_agent::prelude::AgentEvent::Token(_) => {}
                            echo_agent::prelude::AgentEvent::ToolCall { name, args } => {
                                current_tool = Some((name, args));
                            }
                            echo_agent::prelude::AgentEvent::ToolResult { name: _, output } => {
                                if let Some((tool_name, args)) = current_tool.take() {
                                    tool_calls.push(ToolCallInfo {
                                        name: tool_name,
                                        args,
                                        result: output,
                                        success: true,
                                    });
                                }
                            }
                            echo_agent::prelude::AgentEvent::ToolError { name: _, error } => {
                                if let Some((tool_name, args)) = current_tool.take() {
                                    tool_calls.push(ToolCallInfo {
                                        name: tool_name,
                                        args,
                                        result: error,
                                        success: false,
                                    });
                                }
                            }
                            echo_agent::prelude::AgentEvent::FinalAnswer(data) => {
                                final_answer = data;
                            }
                            echo_agent::prelude::AgentEvent::Cancelled => {
                                break;
                            }
                            _ => {}
                        },
                        Err(e) => {
                            let _ = tx.send(Err(WebError::Agent(e)));
                            return;
                        }
                    }
                }

                let (message_count, estimated_tokens) = agent.context_stats().await;
                Ok(Json(ChatResponse {
                    answer: final_answer,
                    tool_calls,
                    iterations: max_iterations,
                    context_stats: ContextStats {
                        message_count,
                        estimated_tokens,
                    },
                }))
            }
            Err(e) => Err(WebError::Agent(e)),
        };

        let _ = tx.send(result);
    });

    rx.await
        .map_err(|_| WebError::Internal("Chat task panicked or was cancelled".to_string()))?
}

/// POST /api/chat/stream - SSE 流式对话
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn handle_chat_stream(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
    let msg = req.message.trim().to_string();
    let session_id = req.session_id.clone();

    // session_id context restore is NOT supported on the shared agent because
    // load_messages() replaces the agent's global context, which would corrupt
    // other concurrent sessions (CLI, WebSocket, other REST requests).
    //
    // When per-session agent isolation is implemented, the session_id will
    // route to a dedicated AgentHandle and restore context there safely.
    if session_id.is_some() {
        tracing::warn!("session_id is not yet supported on the shared agent; ignoring");
    }

    // Spawn agent execution into a channel so the stream is 'static
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<SseEvent, Infallible>>();

    // Validate input — send error directly through the channel if invalid,
    // matching the validation rules of the blocking /api/chat endpoint.
    if msg.is_empty() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Ok(SseEvent::default()
                .event("error")
                .data("{\"error\": \"Message cannot be empty\"}")));
            let _ = tx.send(Ok(SseEvent::default().event("done").data("{}")));
        });
    } else if msg.len() > MAX_MESSAGE_LENGTH {
        let tx = tx.clone();
        let payload = serde_json::json!({"error": format!("Message too long: {} bytes (max {})", msg.len(), MAX_MESSAGE_LENGTH)});
        tokio::spawn(async move {
            let _ = tx.send(Ok(SseEvent::default()
                .event("error")
                .data(payload.to_string())));
            let _ = tx.send(Ok(SseEvent::default().event("done").data("{}")));
        });
    } else {
        let agent_arc = state.connection.agent.inner().clone();

        tokio::spawn(async move {
            let agent = agent_arc.read().await;

            match agent.chat_stream(&msg).await {
                Ok(mut agent_stream) => {
                    while let Some(result) = agent_stream.next().await {
                        let sse_event = match result {
                            Ok(event) => match event {
                                echo_agent::prelude::AgentEvent::Token(data) => {
                                    SseEvent::default().event("token").data(data)
                                }
                                echo_agent::prelude::AgentEvent::ThinkStart => {
                                    SseEvent::default().event("thinking_start").data("{}")
                                }
                                echo_agent::prelude::AgentEvent::ThinkEnd {
                                    prompt_tokens,
                                    completion_tokens,
                                } => {
                                    let payload = serde_json::json!({"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens});
                                    SseEvent::default()
                                        .event("thinking_end")
                                        .data(payload.to_string())
                                }
                                echo_agent::prelude::AgentEvent::ToolCall { name, args } => {
                                    let payload = serde_json::json!({"name": name, "args": args});
                                    SseEvent::default()
                                        .event("tool_call")
                                        .data(payload.to_string())
                                }
                                echo_agent::prelude::AgentEvent::ToolResult { name, output } => {
                                    let payload = serde_json::json!({"name": name, "result": output, "success": true});
                                    SseEvent::default()
                                        .event("tool_result")
                                        .data(payload.to_string())
                                }
                                echo_agent::prelude::AgentEvent::ToolError { name, error } => {
                                    let payload = serde_json::json!({"name": name, "result": error, "success": false});
                                    SseEvent::default()
                                        .event("tool_result")
                                        .data(payload.to_string())
                                }
                                echo_agent::prelude::AgentEvent::FinalAnswer(data) => {
                                    SseEvent::default().event("final_answer").data(data)
                                }
                                echo_agent::prelude::AgentEvent::Cancelled => {
                                    SseEvent::default().event("cancelled").data("{}")
                                }
                                _ => continue,
                            },
                            Err(e) => {
                                let payload = serde_json::json!({"error": e.to_string()});
                                SseEvent::default().event("error").data(payload.to_string())
                            }
                        };
                        if tx.send(Ok(sse_event)).is_err() {
                            break; // client disconnected
                        }
                    }
                }
                Err(e) => {
                    let payload = serde_json::json!({"error": e.to_string()});
                    let _ = tx.send(Ok(SseEvent::default()
                        .event("error")
                        .data(payload.to_string())));
                }
            }
            // Signal completion
            let _ = tx.send(Ok(SseEvent::default().event("done").data("{}")));
        });
    }

    // Build a stream from the channel receiver (single return type for the function)
    let events = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (event, rx))
    });

    Sse::new(events)
}
