//! Tauri IPC commands for chat streaming.
//!
//! Uses `app.emit()` to stream `AgentEvent` items to the frontend,
//! replacing the WebSocket transport from the Axum server.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use chrono::Utc;
use echo_agent::agent::{Agent, CancellationToken};
use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use echo_agent::prelude::AgentEvent;
use echo_agent_app_core::observability::{TraceEvent, TraceKind};
use echo_agent_app_core::tasks::task_runtime::{
    InteractionMode, TaskPlan, TaskRouteDecision, TaskRunStatus, WorkerTraceEvent,
    WorkerTraceEventKind,
};
use futures::StreamExt;
use futures::future::BoxFuture;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
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
    #[serde(rename = "llm_usage")]
    LlmUsage {
        model: String,
        prompt_tokens: usize,
        completion_tokens: usize,
        total_tokens: usize,
        cached_prompt_tokens: usize,
        cache_creation_prompt_tokens: usize,
        usage_reported: bool,
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
    #[serde(rename = "tool_batch_start")]
    ToolBatchStart { tool_count: usize },
    #[serde(rename = "tool_batch_end")]
    ToolBatchEnd,
    #[serde(rename = "chart")]
    Chart { spec: serde_json::Value },
    #[serde(rename = "final_answer")]
    FinalAnswer { data: String },
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "run_status")]
    RunStatus { status: String },
    #[serde(rename = "approval_request")]
    ApprovalRequest {
        request_id: String,
        tool_name: String,
        args: serde_json::Value,
        prompt: String,
    },
    #[serde(rename = "input_request")]
    InputRequest { request_id: String, prompt: String },
    #[serde(rename = "selection_request")]
    SelectionRequest {
        request_id: String,
        prompt: String,
        options: Vec<String>,
        task_id: Option<String>,
        context: Option<serde_json::Value>,
        phase: Option<String>,
    },
    /// A complex-task run was created and a structured plan generated.
    /// The GUI should render the plan + approval actions from the run_id.
    #[serde(rename = "plan_ready")]
    PlanReady {
        run_id: String,
        goal: String,
        domain_profile: String,
        route: String,
        interaction_mode: String,
        permission_mode: String,
        approval_policy: String,
        route_reason: String,
        confidence: f32,
        auto_execute: bool,
        planned_workers: Vec<String>,
        suggested_workers: Vec<String>,
        active_skills: Vec<String>,
        route_signals: Vec<String>,
        classification_signals: Vec<String>,
    },
    #[serde(rename = "done")]
    Done,
}

fn emit_chat_event(
    app: &tauri::AppHandle,
    event: &ChatEvent,
    message_key: &str,
    conversation_id: &Option<String>,
) -> bool {
    let mut payload = match serde_json::to_value(event) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "failed to serialize chat event");
            return false;
        }
    };

    if let serde_json::Value::Object(ref mut map) = payload {
        map.insert(
            "message_key".to_string(),
            serde_json::Value::String(message_key.to_string()),
        );
        map.insert(
            "conversation_id".to_string(),
            conversation_id
                .as_ref()
                .map(|id| serde_json::Value::String(id.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
    }

    app.emit("chat://event", payload).is_ok()
}

fn emit_worker_trace_event(app: &tauri::AppHandle, event: WorkerTraceEvent) -> bool {
    app.emit("worker://trace", event).is_ok()
}

fn chat_trace_event(
    message_key: &str,
    event_type: WorkerTraceEventKind,
    payload: serde_json::Value,
) -> WorkerTraceEvent {
    WorkerTraceEvent::for_worker(message_key, "main", event_type, payload)
        .with_agent("echo-assistant")
        .with_title("Assistant")
}

/// Global pending map for approval/input responses.
#[allow(clippy::type_complexity)]
static PENDING_RESPONSES: LazyLock<Arc<Mutex<HashMap<String, PendingRequest>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

struct PendingRequest {
    message_key: String,
    tx: oneshot::Sender<PendingResponse>,
}

#[derive(Debug)]
enum PendingResponse {
    Approval {
        approved: bool,
        reason: Option<String>,
        scope: Option<String>,
    },
    Input {
        text: String,
    },
    Selection {
        selection: String,
        instructions: Option<String>,
    },
}

/// Tauri-based HumanLoopProvider — emits approval/input requests via Tauri events
/// and awaits responses through the shared PENDING_RESPONSES map.
struct TauriHumanLoopHandler {
    app_handle: tauri::AppHandle,
    pending: Arc<Mutex<HashMap<String, PendingRequest>>>,
    conversation_id: Option<String>,
    message_key: String,
}

impl TauriHumanLoopHandler {
    fn new(
        app_handle: tauri::AppHandle,
        conversation_id: Option<String>,
        message_key: String,
    ) -> Self {
        Self {
            app_handle,
            pending: PENDING_RESPONSES.clone(),
            conversation_id,
            message_key,
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
        let conversation_id = self.conversation_id.clone();
        let message_key = self.message_key.clone();

        Box::pin(async move {
            tracing::debug!(
                request_id = %request_id,
                conversation_id = ?conversation_id,
                message_key = %message_key,
                "Tauri HITL request created"
            );

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
                    pending.lock().await.insert(
                        request_id.clone(),
                        PendingRequest {
                            message_key: message_key.clone(),
                            tx: tx_response,
                        },
                    );
                    emit_chat_event(
                        &app_handle,
                        &ChatEvent::RunStatus {
                            status: "waiting_approval".to_string(),
                        },
                        &message_key,
                        &conversation_id,
                    );
                    let _ = emit_chat_event(&app_handle, &event, &message_key, &conversation_id);

                    tokio::select! {
                        response = rx_response => {
                            match response {
                                Ok(PendingResponse::Approval { approved, reason, scope }) => {
                                    if approved {
                                        match scope.as_deref() {
                                            Some("session_all_tools") => {
                                                Ok(HumanLoopResponse::ApprovedWithScope {
                                                    scope: echo_agent::human_loop::ApprovalScope::SessionAllTools,
                                                })
                                            }
                                            _ => Ok(HumanLoopResponse::Approved),
                                        }
                                    } else {
                                        Ok(HumanLoopResponse::Rejected { reason })
                                    }
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
                    pending.lock().await.insert(
                        request_id.clone(),
                        PendingRequest {
                            message_key: message_key.clone(),
                            tx: tx_response,
                        },
                    );
                    emit_chat_event(
                        &app_handle,
                        &ChatEvent::RunStatus {
                            status: "waiting_input".to_string(),
                        },
                        &message_key,
                        &conversation_id,
                    );
                    let _ = emit_chat_event(&app_handle, &event, &message_key, &conversation_id);

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
                    let event = ChatEvent::SelectionRequest {
                        request_id: request_id.clone(),
                        prompt: req.prompt.clone(),
                        options: req.options.clone().unwrap_or_default(),
                        task_id: req.task_id.clone(),
                        context: req.context.clone(),
                        phase: req.phase.clone(),
                    };
                    pending.lock().await.insert(
                        request_id.clone(),
                        PendingRequest {
                            message_key: message_key.clone(),
                            tx: tx_response,
                        },
                    );
                    emit_chat_event(
                        &app_handle,
                        &ChatEvent::RunStatus {
                            status: "waiting_input".to_string(),
                        },
                        &message_key,
                        &conversation_id,
                    );
                    let _ = emit_chat_event(&app_handle, &event, &message_key, &conversation_id);

                    tokio::select! {
                        response = rx_response => {
                            match response {
                                Ok(PendingResponse::Selection { selection, instructions }) => {
                                    Ok(HumanLoopResponse::Selection { selection, instructions })
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
            }
        })
    }
}

/// Send a chat message and stream agent events via Tauri events.
///
/// When `conversation_id` is provided and an agent pool is active,
/// the message is routed to a pool agent dedicated to that conversation.
/// This enables parallel multi-conversation execution.
#[tauri::command]
pub async fn send_chat_message(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    message: String,
    conversation_id: Option<String>,
    message_key: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    // Route to pool agent if conversation_id is provided and pool is active.
    // First-turn messages can arrive before the GUI has an active conversation;
    // TaskRuntime routing must still be allowed in that case.
    let agent_handle = if let Some(ref conv_id) = conversation_id {
        state.app_state.connection.agent_for(conv_id).await
    } else {
        state.app_state.connection.primary_agent()
    };
    let message_key = message_key.unwrap_or_else(|| Uuid::new_v4().to_string());

    // ── Complex-task router ────────────────────────────────────────────
    // Classify the input. If it looks like a complex, multi-step task, create
    // a TaskRuntime run and generate a structured plan instead of streaming a
    // normal chat. Missing conversation_id is handled by route_complex_task via
    // a message-scoped run id so Welcome-screen first turns still route.
    //
    // InteractionMode: 0=Auto(router), 1=Chat(force normal chat), 2=Task(force TaskRuntime)
    let interaction_mode_raw = state
        .app_state
        .tasks
        .interaction_mode
        .load(Ordering::Relaxed);
    let interaction_mode = match interaction_mode_raw {
        1 => InteractionMode::Chat,
        2 => InteractionMode::Task,
        _ => InteractionMode::Auto,
    };

    if interaction_mode != InteractionMode::Chat {
        let route_llm = agent_handle.read(|a| a.llm_client().cloned()).await;
        let route_feedback = state.app_state.tasks.route_feedback.read().await.clone();
        let route_decision = echo_agent_app_core::tasks::task_runtime::route_message_with_feedback(
            route_llm,
            &message,
            interaction_mode,
            &route_feedback,
        )
        .await;
        if route_decision.route.should_create_runtime_run() {
            let route_label = route_decision.route.as_str();
            // Try to route to TaskRuntime. If routing/planning fails after the
            // router has selected a runtime path, surface that failure instead
            // of silently falling back to normal chat. Silent fallback re-enters
            // the legacy agent-tool path and makes Task/Auto mode look ignored.
            match route_complex_task(
                state.inner(),
                app.clone(),
                message.clone(),
                conversation_id.clone(),
                message_key.clone(),
                interaction_mode,
                route_decision,
            )
            .await
            {
                Ok(result) => return Ok(result),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        route = route_label,
                        "complex-task routing failed"
                    );
                    emit_chat_event(
                        &app,
                        &ChatEvent::Error {
                            message: format!(
                                "TaskRuntime 路由失败，已停止而不是回落到普通 chat：{}",
                                e
                            ),
                        },
                        &message_key,
                        &conversation_id,
                    );
                    return Ok(serde_json::json!({
                        "kind": "complex_task_error",
                        "route": route_label,
                        "error": e.to_string(),
                    }));
                }
            }
        }
    }

    let agent_inner = agent_handle.inner().clone();
    let cancel_token = CancellationToken::new();

    // Register cancel token
    state
        .app_state
        .session
        .cancel_token
        .insert(message_key.clone(), cancel_token.clone());

    // Attach a Tauri HITL handler to this specific agent. This keeps concurrent
    // GUI conversations isolated instead of racing through the global dispatcher.
    let hitl_handler: Arc<dyn HumanLoopProvider> = Arc::new(TauriHumanLoopHandler::new(
        app.clone(),
        conversation_id.clone(),
        message_key.clone(),
    ));
    agent_handle
        .write_async(|agent| {
            let handler = hitl_handler.clone();
            Box::pin(async move {
                agent.set_human_loop_provider_preserving_approvals(handler);
            })
        })
        .await;

    let app_handle = app.clone();
    let cancel_token_for_task = cancel_token.clone();
    let cancel_tokens = state.app_state.session.cancel_token.clone();
    let cleanup_key = message_key.clone();
    let cleanup_agent = agent_handle.clone();
    let event_message_key = message_key.clone();
    let event_conversation_id = conversation_id.clone();
    let trace_collector = state.app_state.trace.collector.clone();
    let trace_session_id = conversation_id
        .clone()
        .unwrap_or_else(|| event_message_key.clone());

    tokio::spawn(async move {
        let start = std::time::Instant::now();
        let mut terminal_status = "completed".to_string();

        emit_chat_event(
            &app_handle,
            &ChatEvent::RunStatus {
                status: "running".to_string(),
            },
            &event_message_key,
            &event_conversation_id,
        );
        emit_worker_trace_event(
            &app_handle,
            chat_trace_event(
                &event_message_key,
                WorkerTraceEventKind::RunStarted,
                serde_json::json!({
                    "conversation_id": event_conversation_id,
                    "mode": "chat"
                }),
            ),
        );
        emit_worker_trace_event(
            &app_handle,
            chat_trace_event(
                &event_message_key,
                WorkerTraceEventKind::WorkerStarted,
                serde_json::json!({
                    "role": "assistant"
                }),
            ),
        );

        // ReactAgent serializes execution internally via execution_mutex.
        //
        // The RwLock read guard is held for the stream's lifetime because
        // `chat_stream_with_cancel` borrows from the agent. This is safe
        // because: (1) with AgentPool, each conversation has its own agent
        // so no cross-conversation contention; (2) writes only happen via
        // `write_async` for config changes, which are rare during streaming.
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
                                AgentEvent::Token(data) => {
                                    emit_worker_trace_event(
                                        &app_handle,
                                        chat_trace_event(
                                            &event_message_key,
                                            WorkerTraceEventKind::WorkerTokenDelta,
                                            serde_json::json!({ "content": data }),
                                        ),
                                    );
                                    ChatEvent::Token { data }
                                }
                                AgentEvent::ThinkStart => {
                                    emit_chat_event(
                                        &app_handle,
                                        &ChatEvent::RunStatus {
                                            status: "thinking".to_string(),
                                        },
                                        &event_message_key,
                                        &event_conversation_id,
                                    );
                                    emit_worker_trace_event(
                                        &app_handle,
                                        chat_trace_event(
                                            &event_message_key,
                                            WorkerTraceEventKind::WorkerThinkingStart,
                                            serde_json::json!({}),
                                        ),
                                    );
                                    ChatEvent::ThinkingStart
                                }
                                AgentEvent::ThinkEnd {
                                    prompt_tokens,
                                    completion_tokens,
                                } => {
                                    emit_worker_trace_event(
                                        &app_handle,
                                        chat_trace_event(
                                            &event_message_key,
                                            WorkerTraceEventKind::WorkerThinkingEnd,
                                            serde_json::json!({
                                                "prompt_tokens": prompt_tokens,
                                                "completion_tokens": completion_tokens
                                            }),
                                        ),
                                    );
                                    ChatEvent::ThinkingEnd {
                                        prompt_tokens,
                                        completion_tokens,
                                    }
                                }
                                AgentEvent::LlmUsage {
                                    model,
                                    prompt_tokens,
                                    completion_tokens,
                                    total_tokens,
                                    cached_prompt_tokens,
                                    cache_creation_prompt_tokens,
                                    usage_reported,
                                } => {
                                    trace_collector
                                        .record(
                                            &trace_session_id,
                                            TraceEvent {
                                                timestamp: Utc::now(),
                                                kind: TraceKind::LlmCall {
                                                    model: model.clone(),
                                                    input_tokens: prompt_tokens as u64,
                                                    output_tokens: completion_tokens as u64,
                                                    cached_input_tokens: cached_prompt_tokens
                                                        as u64,
                                                    cache_creation_input_tokens:
                                                        cache_creation_prompt_tokens as u64,
                                                    usage_reported,
                                                },
                                                duration_ms: None,
                                                metadata: HashMap::from([
                                                    (
                                                        "message_key".to_string(),
                                                        serde_json::json!(
                                                            event_message_key.clone()
                                                        ),
                                                    ),
                                                    (
                                                        "total_tokens".to_string(),
                                                        serde_json::json!(total_tokens),
                                                    ),
                                                ]),
                                            },
                                        )
                                        .await;
                                    emit_worker_trace_event(
                                        &app_handle,
                                        chat_trace_event(
                                            &event_message_key,
                                            WorkerTraceEventKind::WorkerLlmUsage,
                                            serde_json::json!({
                                                "model": model.clone(),
                                                "prompt_tokens": prompt_tokens,
                                                "completion_tokens": completion_tokens,
                                                "total_tokens": total_tokens,
                                                "cached_prompt_tokens": cached_prompt_tokens,
                                                "cache_creation_prompt_tokens": cache_creation_prompt_tokens,
                                                "usage_reported": usage_reported
                                            }),
                                        ),
                                    );
                                    ChatEvent::LlmUsage {
                                        model: model.clone(),
                                        prompt_tokens,
                                        completion_tokens,
                                        total_tokens,
                                        cached_prompt_tokens,
                                        cache_creation_prompt_tokens,
                                        usage_reported,
                                    }
                                }
                                AgentEvent::ToolCall { name, args } => {
                                    emit_chat_event(
                                        &app_handle,
                                        &ChatEvent::RunStatus {
                                            status: "using_tool".to_string(),
                                        },
                                        &event_message_key,
                                        &event_conversation_id,
                                    );
                                    emit_worker_trace_event(
                                        &app_handle,
                                        chat_trace_event(
                                            &event_message_key,
                                            WorkerTraceEventKind::WorkerToolStart,
                                            serde_json::json!({
                                                "name": name,
                                                "args": args
                                            }),
                                        ),
                                    );
                                    ChatEvent::ToolStart { name, args }
                                }
                                AgentEvent::ToolResult { name, output } => {
                                    emit_worker_trace_event(
                                        &app_handle,
                                        chat_trace_event(
                                            &event_message_key,
                                            WorkerTraceEventKind::WorkerToolResult,
                                            serde_json::json!({
                                                "name": name,
                                                "result": output,
                                                "success": true
                                            }),
                                        ),
                                    );
                                    ChatEvent::ToolResult {
                                        name,
                                        result: output,
                                        success: true,
                                    }
                                }
                                AgentEvent::ToolError { name, error } => {
                                    emit_worker_trace_event(
                                        &app_handle,
                                        chat_trace_event(
                                            &event_message_key,
                                            WorkerTraceEventKind::WorkerToolResult,
                                            serde_json::json!({
                                                "name": name,
                                                "result": error,
                                                "success": false
                                            }),
                                        ),
                                    );
                                    ChatEvent::ToolResult {
                                        name,
                                        result: error,
                                        success: false,
                                    }
                                }
                                AgentEvent::ToolBatchStart { tool_count } => {
                                    ChatEvent::ToolBatchStart { tool_count }
                                }
                                AgentEvent::ToolBatchEnd => ChatEvent::ToolBatchEnd,
                                AgentEvent::Chart { spec } => ChatEvent::Chart { spec },
                                AgentEvent::FinalAnswer(data) => {
                                    terminal_status = "completed".to_string();
                                    emit_worker_trace_event(
                                        &app_handle,
                                        chat_trace_event(
                                            &event_message_key,
                                            WorkerTraceEventKind::WorkerCompleted,
                                            serde_json::json!({}),
                                        ),
                                    );
                                    ChatEvent::FinalAnswer { data }
                                }
                                AgentEvent::Cancelled => {
                                    terminal_status = "cancelled".to_string();
                                    emit_worker_trace_event(
                                        &app_handle,
                                        chat_trace_event(
                                            &event_message_key,
                                            WorkerTraceEventKind::WorkerCancelled,
                                            serde_json::json!({}),
                                        ),
                                    );
                                    ChatEvent::Cancelled
                                }
                                AgentEvent::Error { source, message } => {
                                    terminal_status = "failed".to_string();
                                    emit_worker_trace_event(
                                        &app_handle,
                                        chat_trace_event(
                                            &event_message_key,
                                            WorkerTraceEventKind::WorkerFailed,
                                            serde_json::json!({
                                                "source": source,
                                                "message": message
                                            }),
                                        ),
                                    );
                                    ChatEvent::Error {
                                        message: format!("{source}: {message}"),
                                    }
                                }
                                _ => continue,
                            };

                            if !emit_chat_event(
                                &app_handle,
                                &chat_event,
                                &event_message_key,
                                &event_conversation_id,
                            ) {
                                break;
                            }
                        }
                        Err(e) => {
                            terminal_status = "failed".to_string();
                            let _ = emit_chat_event(
                                &app_handle,
                                &ChatEvent::Error {
                                    message: e.to_string(),
                                },
                                &event_message_key,
                                &event_conversation_id,
                            );
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                terminal_status = "failed".to_string();
                let _ = emit_chat_event(
                    &app_handle,
                    &ChatEvent::Error {
                        message: e.to_string(),
                    },
                    &event_message_key,
                    &event_conversation_id,
                );
            }
        }

        // Emit done event
        let terminal_trace_kind = match terminal_status.as_str() {
            "completed" => WorkerTraceEventKind::RunCompleted,
            "cancelled" => WorkerTraceEventKind::RunCancelled,
            "failed" => WorkerTraceEventKind::RunFailed,
            _ => WorkerTraceEventKind::RunStatusChanged,
        };
        emit_worker_trace_event(
            &app_handle,
            chat_trace_event(
                &event_message_key,
                terminal_trace_kind,
                serde_json::json!({
                    "status": terminal_status.clone()
                }),
            ),
        );
        let _ = emit_chat_event(
            &app_handle,
            &ChatEvent::RunStatus {
                status: terminal_status,
            },
            &event_message_key,
            &event_conversation_id,
        );
        let _ = emit_chat_event(
            &app_handle,
            &ChatEvent::Done,
            &event_message_key,
            &event_conversation_id,
        );

        // Cleanup: restore an empty dispatcher (NOT the REPL-laden runtime
        // dispatcher) so the agent doesn't fall back to terminal-blocking
        // approval between messages.
        cancel_tokens.remove(&cleanup_key);
        cleanup_agent
            .write_async(|agent| {
                Box::pin(async move {
                    let empty = Arc::new(echo_agent_app_core::hitl::HitlDispatcher::new());
                    agent.set_human_loop_provider_preserving_approvals(empty);
                })
            })
            .await;

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
    message_key: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    if let Some(ref key) = message_key {
        if let Some((_, token)) = state.app_state.session.cancel_token.remove(key) {
            token.cancel();
        }
    } else {
        // Fallback for non-isolated callers.
        for entry in state.app_state.session.cancel_token.iter() {
            entry.value().cancel();
        }
        state.app_state.session.cancel_token.clear();
    }

    // Reject all pending approval requests so parked HITL futures unblock
    // immediately instead of waiting up to 300s for a timeout.
    let mut pending = PENDING_RESPONSES.lock().await;
    let request_ids: Vec<String> = pending
        .iter()
        .filter_map(|(request_id, req)| {
            if message_key
                .as_ref()
                .map(|key| &req.message_key == key)
                .unwrap_or(true)
            {
                Some(request_id.clone())
            } else {
                None
            }
        })
        .collect();

    for request_id in request_ids {
        let Some(req) = pending.remove(&request_id) else {
            continue;
        };
        let _ = req.tx.send(PendingResponse::Approval {
            approved: false,
            reason: Some("cancelled by user".to_string()),
            scope: None,
        });
        tracing::debug!(%request_id, "cancelled pending approval on cancel_chat");
    }

    Ok(serde_json::json!({"success": true}))
}

/// Respond to an approval request.
#[tauri::command]
pub async fn send_approval_response(
    request_id: String,
    approved: bool,
    reason: Option<String>,
    scope: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let req = PENDING_RESPONSES.lock().await.remove(&request_id);
    if let Some(req) = req {
        let _ = req.tx.send(PendingResponse::Approval {
            approved,
            reason,
            scope,
        });
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
    let req = PENDING_RESPONSES.lock().await.remove(&request_id);
    if let Some(req) = req {
        let _ = req.tx.send(PendingResponse::Input { text });
        Ok(serde_json::json!({"success": true}))
    } else {
        Err(IpcError::NotFound(format!(
            "Input request '{}' not found or expired",
            request_id
        )))
    }
}

/// Respond to a selection request.
#[tauri::command]
pub async fn send_selection_response(
    request_id: String,
    selection: String,
    instructions: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let req = PENDING_RESPONSES.lock().await.remove(&request_id);
    if let Some(req) = req {
        let _ = req.tx.send(PendingResponse::Selection {
            selection,
            instructions,
        });
        Ok(serde_json::json!({"success": true}))
    } else {
        Err(IpcError::NotFound(format!(
            "Selection request '{}' not found or expired",
            request_id
        )))
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Complex-task router (PR 2)
// ══════════════════════════════════════════════════════════════════════════

/// Handle a complex input by creating a TaskRuntime run and generating a
/// structured plan. Emits a `plan_ready` chat event so the GUI can render the
/// plan and approval actions. The run stops at `AwaitingPlanApproval` — the
/// user must approve before execution (PR 3).
///
/// Returns a JSON object with `kind: "complex_task"`, `run_id`, and the plan
/// so an IPC caller that doesn't listen on `chat://event` still gets the data.
async fn route_complex_task(
    state: &TauriState,
    app: tauri::AppHandle,
    message: String,
    conversation_id: Option<String>,
    message_key: String,
    interaction_mode: InteractionMode,
    route_decision: TaskRouteDecision,
) -> Result<serde_json::Value, anyhow::Error> {
    let store = state
        .app_state
        .tasks
        .runtime
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("TaskRuntime store not initialized"))?
        .clone();

    let conv_id = conversation_id
        .clone()
        .unwrap_or_else(|| format!("message:{message_key}"));
    let permission_mode = state.app_state.config.permission_mode.read().await.clone();

    // 1. Create the run in Pending.
    let run_id = uuid::Uuid::new_v4().to_string();
    store.create_run(
        &run_id,
        "default", // workspace_id — resolved properly in PR 6 workspace wiring
        &conv_id,
        "", // root_message_id — linked in PR 6
        route_decision.classification.inferred_profile,
        &message,
    )?;

    // 2. Pending -> Planning (legal direct transition).
    store.transition_run(&run_id, TaskRunStatus::Planning)?;

    // 3. Generate the structured plan. Broad read-only fanout must be a
    // reliable runtime path, so it is built deterministically from the router
    // decision instead of depending on a second LLM planning call.
    let generated = if route_decision.route
        == echo_agent_app_core::tasks::task_runtime::TaskRouteKind::ParallelReadonlyDelegation
    {
        echo_agent_app_core::tasks::task_runtime::generate_parallel_readonly_plan(
            &run_id,
            &message,
            &route_decision.classification,
            &route_decision.suggested_workers,
        )
    } else {
        let llm = state
            .app_state
            .connection
            .primary_agent()
            .read(|a| a.llm_client().cloned())
            .await
            .ok_or_else(|| anyhow::anyhow!("no LLM client available on primary agent"))?;
        echo_agent_app_core::tasks::task_runtime::generate_plan(
            &llm,
            &run_id,
            &message,
            &route_decision.classification,
            &route_decision.suggested_workers,
        )
        .await?
    };

    // 4. Persist + advance to AwaitingPlanApproval (attach_plan is atomic).
    store.attach_plan(&generated.plan)?;
    let planned_workers = planned_worker_roles(&generated.plan);
    let mut auto_execute = false;
    let mut response_status = TaskRunStatus::AwaitingPlanApproval;
    if route_decision.route.should_auto_execute()
        && generated
            .plan
            .tasks
            .iter()
            .all(|task| task.kind.is_read_only())
    {
        store.transition_run(&run_id, TaskRunStatus::Ready)?;
        launch_task_run_execution(state, app.clone(), &run_id).await?;
        auto_execute = true;
        response_status = TaskRunStatus::Running;
    }

    // 5. Emit plan_ready so the GUI can render the plan + approval actions.
    let active_skills = state
        .app_state
        .connection
        .primary_agent()
        .read(|agent| agent.skill_registry().activated_names())
        .await;
    emit_chat_event(
        &app,
        &ChatEvent::PlanReady {
            run_id: run_id.clone(),
            goal: generated.plan.goal.clone(),
            domain_profile: route_decision
                .classification
                .inferred_profile
                .as_str()
                .to_string(),
            route: route_decision.route.as_str().to_string(),
            interaction_mode: interaction_mode.as_str().to_string(),
            permission_mode: permission_mode.clone(),
            approval_policy: approval_policy_summary(
                &permission_mode,
                route_decision.route.should_auto_execute(),
                auto_execute,
            )
            .to_string(),
            route_reason: route_decision.reason.clone(),
            confidence: route_decision.confidence,
            auto_execute,
            planned_workers,
            suggested_workers: route_decision.suggested_workers.clone(),
            active_skills,
            route_signals: route_decision
                .reason
                .split("routing_signals:")
                .nth(1)
                .map(|value| value.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default(),
            classification_signals: route_decision.classification.signals.clone(),
        },
        // Use message_key so first-turn messages without an active
        // conversation_id still pass the frontend event guard.
        &message_key,
        &conversation_id,
    );

    tracing::info!(
        run_id = %run_id,
        plan_id = %generated.plan.plan_id,
        task_count = generated.plan.tasks.len(),
        route = ?route_decision.route,
        auto_execute,
        "task routed to TaskRuntime"
    );

    Ok(serde_json::json!({
        "kind": "complex_task",
        "run_id": run_id,
        "status": response_status.as_str(),
        "route": route_decision.route,
        "auto_execute": auto_execute,
        "conversation_id": conv_id,
        "plan": generated.plan,
        "warnings": generated.warnings,
    }))
}

fn planned_worker_roles(plan: &TaskPlan) -> Vec<String> {
    let mut workers = Vec::new();
    for task in &plan.tasks {
        if task.agent_role.trim().is_empty() {
            continue;
        }
        if !workers.iter().any(|worker| worker == &task.agent_role) {
            workers.push(task.agent_role.clone());
        }
    }
    workers
}

fn approval_policy_summary(
    permission_mode: &str,
    route_auto_execute: bool,
    auto_execute: bool,
) -> &'static str {
    if auto_execute {
        "只读并行任务已自动执行；后续工具审批仍由当前审批模式控制"
    } else if route_auto_execute {
        "已识别为只读并行路径，但计划包含需确认步骤，等待用户确认"
    } else {
        match permission_mode {
            "full-auto" => "工具操作默认自动通过，高风险保护仍会拦截",
            "auto-edit" => "读取和编辑类操作自动通过，高风险操作会询问",
            "strict" => "写入、命令、网络等敏感操作都会询问",
            _ => "高风险操作会询问，计划执行前需要用户确认",
        }
    }
}

async fn launch_task_run_execution(
    state: &TauriState,
    app: tauri::AppHandle,
    run_id: &str,
) -> Result<(), anyhow::Error> {
    let store = state
        .app_state
        .tasks
        .runtime
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("TaskRuntime store not initialized"))?
        .clone();
    let primary_agent = state.app_state.connection.primary_agent();

    let store_for_task = store.clone();
    let primary_agent_for_task = primary_agent.clone();
    let run_store_for_task = primary_agent.read(|a| a.run_store().cloned()).await;
    let reviewer_llm = primary_agent.read(|a| a.llm_client().cloned()).await;
    let layer_manager = state
        .app_state
        .review_integration
        .as_ref()
        .map(|ri| std::sync::Arc::new(ri.create_layer_manager()));
    let cancel = echo_agent::agent::CancellationToken::new();
    let run_id_for_task = run_id.to_string();
    let run_key = format!("__run__:{run_id_for_task}");
    state
        .app_state
        .tasks
        .run_cancel_tokens
        .insert(run_key, cancel.clone());
    let run_cancel_tokens = state.app_state.tasks.run_cancel_tokens.clone();
    let trace_sink: echo_agent_app_core::tasks::task_runtime::WorkerTraceSink =
        Arc::new(move |event| {
            let _ = app.emit("worker://trace", event);
        });

    tokio::spawn(async move {
        let outcome = echo_agent_app_core::tasks::task_runtime::execute_run(
            store_for_task.clone(),
            Some(primary_agent_for_task),
            reviewer_llm,
            layer_manager,
            run_store_for_task,
            Some(trace_sink),
            &run_id_for_task,
            cancel,
        )
        .await;
        run_cancel_tokens.remove(&format!("__run__:{run_id_for_task}"));
        match outcome {
            Ok(echo_agent_app_core::tasks::task_runtime::RunOutcome::Completed) => {
                tracing::info!(run_id = %run_id_for_task, "auto-routed run completed");
            }
            Ok(other) => {
                tracing::warn!(run_id = %run_id_for_task, ?other, "auto-routed run ended non-completed");
            }
            Err(e) => {
                tracing::error!(run_id = %run_id_for_task, error = %e, "auto-routed run executor error");
            }
        }
    });

    Ok(())
}
