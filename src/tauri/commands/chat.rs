//! Tauri IPC commands for chat streaming.
//!
//! Uses `app.emit()` to stream `AgentEvent` items to the frontend,
//! replacing the WebSocket transport from the Axum server.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::agent::{Agent, CancellationToken};
use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use echo_agent::prelude::AgentEvent;
use futures::StreamExt;
use futures::future::BoxFuture;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tauri::Emitter;
use tokio::sync::{Mutex, oneshot};
use uuid::Uuid;

/// Event payload emitted to the frontend via `app.emit("chat://event", ...)`.
#[derive(Clone, Serialize)]
#[serde(tag = "type")]
pub enum ChatEvent {
    #[serde(rename = "token")]
    Token { data: String },
    #[serde(rename = "thinking_start")]
    ThinkingStart,
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        prompt_tokens: usize,
        completion_tokens: usize,
    },
    #[serde(rename = "tool_start")]
    ToolStart {
        name: String,
        args: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        name: String,
        result: String,
        success: bool,
    },
    #[serde(rename = "chart")]
    Chart { spec: serde_json::Value },
    #[serde(rename = "final_answer")]
    FinalAnswer { data: String },
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "approval_request")]
    ApprovalRequest {
        request_id: String,
        tool_name: String,
        args: serde_json::Value,
        prompt: String,
    },
    #[serde(rename = "input_request")]
    InputRequest { request_id: String, prompt: String },
    #[serde(rename = "done")]
    Done,
}

/// Global pending map for approval/input responses.
static PENDING_RESPONSES: LazyLock<Arc<Mutex<HashMap<String, oneshot::Sender<PendingResponse>>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

#[derive(Debug)]
enum PendingResponse {
    Approval {
        approved: bool,
        reason: Option<String>,
    },
    Input {
        text: String,
    },
}

/// Tauri-based HumanLoopProvider — emits approval/input requests via Tauri events
/// and awaits responses through the shared PENDING_RESPONSES map.
struct TauriHumanLoopHandler {
    app_handle: tauri::AppHandle,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<PendingResponse>>>>,
}

impl TauriHumanLoopHandler {
    fn new(app_handle: tauri::AppHandle) -> Self {
        Self {
            app_handle,
            pending: PENDING_RESPONSES.clone(),
        }
    }
}

impl HumanLoopProvider for TauriHumanLoopHandler {
    fn request(
        &self,
        req: HumanLoopRequest,
    ) -> BoxFuture<'_, echo_agent::error::Result<HumanLoopResponse>> {
        let request_id = Uuid::new_v4().to_string();
        let (tx_response, rx_response) = oneshot::channel();
        let app_handle = self.app_handle.clone();
        let pending = self.pending.clone();

        Box::pin(async move {
            match req.kind {
                echo_agent::human_loop::HumanLoopKind::Approval => {
                    let tool_name = req.tool_name.clone().unwrap_or_default();
                    let args = req.args.clone().unwrap_or(serde_json::Value::Null);
                    let event = ChatEvent::ApprovalRequest {
                        request_id: request_id.clone(),
                        tool_name,
                        args,
                        prompt: req.prompt.clone(),
                    };
                    let _ = app_handle.emit("chat://event", &event);
                    pending.lock().await.insert(request_id.clone(), tx_response);

                    tokio::select! {
                        response = rx_response => {
                            match response {
                                Ok(PendingResponse::Approval { approved, reason }) => {
                                    if approved { Ok(HumanLoopResponse::Approved) }
                                    else { Ok(HumanLoopResponse::Rejected { reason }) }
                                }
                                _ => Ok(HumanLoopResponse::Timeout),
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {
                            pending.lock().await.remove(&request_id);
                            Ok(HumanLoopResponse::Timeout)
                        }
                    }
                }
                echo_agent::human_loop::HumanLoopKind::Input => {
                    let event = ChatEvent::InputRequest {
                        request_id: request_id.clone(),
                        prompt: req.prompt.clone(),
                    };
                    let _ = app_handle.emit("chat://event", &event);
                    pending.lock().await.insert(request_id.clone(), tx_response);

                    tokio::select! {
                        response = rx_response => {
                            match response {
                                Ok(PendingResponse::Input { text }) => Ok(HumanLoopResponse::Text(text)),
                                _ => Ok(HumanLoopResponse::Text(String::new())),
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {
                            pending.lock().await.remove(&request_id);
                            Ok(HumanLoopResponse::Text(String::new()))
                        }
                    }
                }
                echo_agent::human_loop::HumanLoopKind::Selection => {
                    // For now, auto-approve with the first option
                    // TODO: implement proper selection UI in frontend
                    let selection = req
                        .options
                        .and_then(|opts| opts.into_iter().next())
                        .unwrap_or_else(|| "approve".to_string());
                    Ok(HumanLoopResponse::Selection {
                        selection,
                        instructions: None,
                    })
                }
            }
        })
    }
}

/// Send a chat message and stream agent events via Tauri events.
#[tauri::command]
pub async fn send_chat_message(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    message: String,
) -> Result<serde_json::Value, IpcError> {
    let agent_inner = state.app_state.connection.agent.inner().clone();
    let cancel_token = CancellationToken::new();
    let message_key = Uuid::new_v4().to_string();

    // Register cancel token
    state
        .app_state
        .session
        .cancel_token
        .insert(message_key.clone(), cancel_token.clone());

    // Register Tauri HITL handler with the dispatcher (before spawning)
    let hitl_handler: Arc<dyn HumanLoopProvider> =
        Arc::new(TauriHumanLoopHandler::new(app.clone()));
    state
        .app_state
        .connection
        .hitl_dispatcher
        .register("tauri", hitl_handler)
        .await;

    let app_handle = app.clone();
    let cancel_token_for_task = cancel_token.clone();
    let hitl_dispatcher = state.app_state.connection.hitl_dispatcher.clone();
    let cancel_tokens = state.app_state.session.cancel_token.clone();
    let cleanup_key = message_key.clone();

    tokio::spawn(async move {
        let start = std::time::Instant::now();

        // ReactAgent serializes execution internally via execution_mutex

        // Acquire read lock — must be held while stream is consumed
        let agent = agent_inner.read().await;
        let stream_result = agent
            .chat_stream_with_cancel(&message, cancel_token_for_task)
            .await;

        match stream_result {
            Ok(mut stream) => {
                while let Some(event_result) = stream.next().await {
                    match event_result {
                        Ok(event) => {
                            let chat_event = match event {
                                AgentEvent::Token(data) => ChatEvent::Token { data },
                                AgentEvent::ThinkStart => ChatEvent::ThinkingStart,
                                AgentEvent::ThinkEnd {
                                    prompt_tokens,
                                    completion_tokens,
                                } => ChatEvent::ThinkingEnd {
                                    prompt_tokens,
                                    completion_tokens,
                                },
                                AgentEvent::ToolCall { name, args } => {
                                    ChatEvent::ToolStart { name, args }
                                }
                                AgentEvent::ToolResult { name, output } => ChatEvent::ToolResult {
                                    name,
                                    result: output,
                                    success: true,
                                },
                                AgentEvent::ToolError { name, error } => ChatEvent::ToolResult {
                                    name,
                                    result: error,
                                    success: false,
                                },
                                AgentEvent::Chart { spec } => ChatEvent::Chart { spec },
                                AgentEvent::FinalAnswer(data) => ChatEvent::FinalAnswer { data },
                                AgentEvent::Cancelled => ChatEvent::Cancelled,
                                AgentEvent::Error { source, message } => ChatEvent::Error {
                                    message: format!("{source}: {message}"),
                                },
                                _ => continue,
                            };

                            if app_handle.emit("chat://event", &chat_event).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = app_handle.emit(
                                "chat://event",
                                &ChatEvent::Error {
                                    message: e.to_string(),
                                },
                            );
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = app_handle.emit(
                    "chat://event",
                    &ChatEvent::Error {
                        message: e.to_string(),
                    },
                );
            }
        }

        // Emit done event
        let _ = app_handle.emit("chat://event", &ChatEvent::Done);

        // Cleanup
        cancel_tokens.remove(&cleanup_key);
        hitl_dispatcher.unregister("tauri").await;

        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            "Tauri chat stream finished"
        );
    });

    Ok(serde_json::json!({
        "success": true,
        "message_key": message_key,
    }))
}

/// Cancel an active chat stream.
#[tauri::command]
pub async fn cancel_chat(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    for entry in state.app_state.session.cancel_token.iter() {
        entry.value().cancel();
    }
    state.app_state.session.cancel_token.clear();
    Ok(serde_json::json!({"success": true}))
}

/// Respond to an approval request.
#[tauri::command]
pub async fn send_approval_response(
    request_id: String,
    approved: bool,
    reason: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let tx = PENDING_RESPONSES.lock().await.remove(&request_id);
    if let Some(tx) = tx {
        let _ = tx.send(PendingResponse::Approval { approved, reason });
        Ok(serde_json::json!({"success": true}))
    } else {
        Err(IpcError::NotFound(format!(
            "Approval request '{}' not found or expired",
            request_id
        )))
    }
}

/// Respond to an input request.
#[tauri::command]
pub async fn send_input_response(
    request_id: String,
    text: String,
) -> Result<serde_json::Value, IpcError> {
    let tx = PENDING_RESPONSES.lock().await.remove(&request_id);
    if let Some(tx) = tx {
        let _ = tx.send(PendingResponse::Input { text });
        Ok(serde_json::json!({"success": true}))
    } else {
        Err(IpcError::NotFound(format!(
            "Input request '{}' not found or expired",
            request_id
        )))
    }
}
