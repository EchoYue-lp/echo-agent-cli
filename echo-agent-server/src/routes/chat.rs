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
#[debug_handler]
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

    state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                let max_iterations = agent.config().get_max_iterations();
                let mut final_answer = String::new();
                let mut tool_calls = Vec::new();

                {
                    let stream_result = agent.chat_stream(&req.message).await;
                    let mut stream = stream_result?;
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
                                return Err(WebError::Agent(e));
                            }
                        }
                    }
                }

                let (message_count, estimated_tokens) = agent.context_stats().await;
                let context_stats = ContextStats {
                    message_count,
                    estimated_tokens,
                };

                Ok(Json(ChatResponse {
                    answer: final_answer,
                    tool_calls,
                    iterations: max_iterations,
                    context_stats,
                }))
            })
        })
        .await
}

/// POST /api/chat/stream - SSE 流式对话
#[debug_handler]
pub async fn handle_chat_stream(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
    let msg = req.message.trim().to_string();
    let events = if msg.is_empty() {
        // Return a single error event for empty messages
        let error_event = SseEvent::default()
            .event("error")
            .data("{\"error\": \"Message cannot be empty\"}");
        stream::once(async move { Ok(error_event) }).left_stream()
    } else {
        // Spawn agent execution into a channel so the stream is 'static
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<SseEvent, Infallible>>();
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

        // Build a stream from the channel receiver
        stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .right_stream()
    };

    Sse::new(events)
}
