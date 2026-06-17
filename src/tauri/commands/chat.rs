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
use std::sync::atomic::Ordering;
use echo_agent_app_core::tasks::task_runtime::TaskRunStatus;
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
        signals: Vec<String>,
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
    // Route to pool agent if conversation_id is provided and pool is active
    let agent_handle = if let Some(ref conv_id) = conversation_id {
        state.app_state.connection.agent_for(conv_id).await
    } else {
        state.app_state.connection.primary_agent()
    };
    let message_key = message_key.unwrap_or_else(|| Uuid::new_v4().to_string());

    // ── Complex-task router ────────────────────────────────────────────
    // Classify the input. If it looks like a complex, multi-step task AND a
    // conversation_id is available, create a TaskRuntime run and generate a
    // structured plan instead of streaming a normal chat. The run stops at
    // `AwaitingPlanApproval` — the user must approve before execution (PR 3).
    //
    // Safety: auto-routing is gated by a runtime flag on TaskState
    // (`auto_route`, default OFF). Without it the router is inert and every
    // message takes the normal chat path, preserving today's behavior
    // exactly. The GUI toggles it via `set_taskruntime_auto_route` and can
    // also drive runs explicitly via create_task_run / generate_task_plan.
    let auto_route_enabled = state.app_state.tasks.auto_route.load(Ordering::Relaxed);
    // InteractionMode: 0=Auto(heuristic), 1=Chat(force normal chat), 2=Plan(force planning)
    let interaction_mode = state.app_state.tasks.interaction_mode.load(Ordering::Relaxed);

    let should_route = match interaction_mode {
        1 => false, // Chat mode: never route to TaskRuntime
        2 => true,  // Plan mode: always route
        _ => auto_route_enabled, // Auto: defer to classifier + auto_route flag
    };
    let force_complex = interaction_mode == 2;

    if should_route && conversation_id.is_some() {
        let classification = if force_complex {
            // Plan mode: fabricate a Complex classification
            echo_agent_app_core::tasks::task_runtime::Classification {
                complexity: echo_agent_app_core::tasks::task_runtime::ComplexityLabel::Complex,
                inferred_profile: echo_agent_app_core::tasks::task_runtime::DomainProfile::General,
                reason: "forced by Plan mode".into(),
                signals: vec!["plan_mode".into()],
            }
        } else {
            echo_agent_app_core::tasks::task_runtime::HeuristicClassifier::new()
                .classify(&message)
        };
        if classification.complexity
            == echo_agent_app_core::tasks::task_runtime::ComplexityLabel::Complex
        {
            // Try to route to TaskRuntime. On any failure, log and FALL THROUGH
            // to the normal chat path — the user's message must not vanish.
            match route_complex_task(state.inner(), app.clone(), message.clone(), conversation_id.clone(), classification).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    tracing::warn!(error = %e, "complex-task routing failed; falling back to normal chat");
                    // Fall through to normal chat below.
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
                                AgentEvent::Token(data) => ChatEvent::Token { data },
                                AgentEvent::ThinkStart => {
                                    emit_chat_event(
                                        &app_handle,
                                        &ChatEvent::RunStatus {
                                            status: "thinking".to_string(),
                                        },
                                        &event_message_key,
                                        &event_conversation_id,
                                    );
                                    ChatEvent::ThinkingStart
                                }
                                AgentEvent::ThinkEnd {
                                    prompt_tokens,
                                    completion_tokens,
                                } => ChatEvent::ThinkingEnd {
                                    prompt_tokens,
                                    completion_tokens,
                                },
                                AgentEvent::ToolCall { name, args } => {
                                    emit_chat_event(
                                        &app_handle,
                                        &ChatEvent::RunStatus {
                                            status: "using_tool".to_string(),
                                        },
                                        &event_message_key,
                                        &event_conversation_id,
                                    );
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
                                AgentEvent::ToolBatchStart { tool_count } => {
                                    ChatEvent::ToolBatchStart { tool_count }
                                }
                                AgentEvent::ToolBatchEnd => ChatEvent::ToolBatchEnd,
                                AgentEvent::Chart { spec } => ChatEvent::Chart { spec },
                                AgentEvent::FinalAnswer(data) => {
                                    terminal_status = "completed".to_string();
                                    ChatEvent::FinalAnswer { data }
                                }
                                AgentEvent::Cancelled => {
                                    terminal_status = "cancelled".to_string();
                                    ChatEvent::Cancelled
                                }
                                AgentEvent::Error { source, message } => {
                                    terminal_status = "failed".to_string();
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
    classification: echo_agent_app_core::tasks::task_runtime::Classification,
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
        .ok_or_else(|| anyhow::anyhow!("complex-task routing requires a conversation_id"))?;

    // 1. Create the run in Pending.
    let run_id = uuid::Uuid::new_v4().to_string();
    let run = store.create_run(
        &run_id,
        "default", // workspace_id — resolved properly in PR 6 workspace wiring
        &conv_id,
        "", // root_message_id — linked in PR 6
        classification.inferred_profile,
        &message,
    )?;

    // 2. Pending -> Planning (legal direct transition).
    store.transition_run(&run_id, TaskRunStatus::Planning)?;

    // 3. Obtain the LLM client from the primary agent.
    let llm = state
        .app_state
        .connection
        .primary_agent()
        .read(|a| a.llm_client().cloned())
        .await
        .ok_or_else(|| anyhow::anyhow!("no LLM client available on primary agent"))?;

    // 4. Generate the structured plan.
    let generated =
        echo_agent_app_core::tasks::task_runtime::generate_plan(&llm, &run_id, &message, &classification)
            .await?;

    // 5. Persist + advance to AwaitingPlanApproval (attach_plan is atomic).
    store.attach_plan(&generated.plan)?;

    // 6. Emit plan_ready so the GUI can render the plan + approval actions.
    emit_chat_event(
        &app,
        &ChatEvent::PlanReady {
            run_id: run_id.clone(),
            goal: generated.plan.goal.clone(),
            domain_profile: classification.inferred_profile.as_str().to_string(),
            signals: classification.signals.clone(),
        },
        // Empty message_key so the frontend's isCurrentRunEvent guard falls
        // through to the conversation_id match — PlanReady is a run-scoped
        // event, not tied to a specific streaming message.
        "",
        &conversation_id,
    );

    tracing::info!(
        run_id = %run_id,
        plan_id = %generated.plan.plan_id,
        task_count = generated.plan.tasks.len(),
        "complex task routed to TaskRuntime; awaiting plan approval"
    );

    Ok(serde_json::json!({
        "kind": "complex_task",
        "run_id": run_id,
        "status": run.status.as_str(),
        "plan": generated.plan,
        "warnings": generated.warnings,
    }))
}
