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
use echo_agent_app_core::tasks::conversation_runtime::ConversationRuntimeEvent;
use echo_agent_app_core::tasks::task_runtime::{
    AttendedMode, ExecutionPolicy, InteractionMode, TaskRouteDecision, TaskRouteKind,
    TaskRunStatus, WorkerTraceEvent, WorkerTraceEventKind,
};
use futures::StreamExt;
use futures::future::BoxFuture;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock};
use tauri::Emitter;
use tokio::sync::{Mutex, oneshot};
use uuid::Uuid;

/// Compute a content fingerprint hash (first 16 hex chars of SHA-256).
fn compute_content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(&hasher.finalize()[..8])
}

/// Emit a unified conversation runtime event to the frontend.
fn emit_conversation_event(
    app: &tauri::AppHandle,
    event: &echo_agent_app_core::tasks::conversation_runtime::ConversationRuntimeEvent,
    conversation_id: &str,
    store: Option<&std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
) {
    let payload = serde_json::json!({
        "conversation_id": conversation_id,
        "event": event,
    });
    let _ = app.emit("conversation://event", payload);

    // Persist for replay on history refresh
    if let Some(store) = store {
        let event_type = match event {
            ConversationRuntimeEvent::RouteDecision { .. } => "route_decision",
            ConversationRuntimeEvent::InitialThinking { .. } => "initial_thinking",
            ConversationRuntimeEvent::WorkerStarted { .. } => "worker_started",
            ConversationRuntimeEvent::WorkerToolCall { .. } => "worker_tool_call",
            ConversationRuntimeEvent::WorkerResult { .. } => "worker_result",
            ConversationRuntimeEvent::LlmUsage { .. } => "llm_usage",
            ConversationRuntimeEvent::FinalAnswer { .. } => "final_answer",
            ConversationRuntimeEvent::ApprovalRequest { .. } => "approval_request",
            ConversationRuntimeEvent::Error { .. } => "error",
        };
        let _ = store.append_conversation_event(
            conversation_id,
            event_type,
            &serde_json::to_string(event).unwrap_or_default(),
        );
    }
}

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
    #[serde(rename = "done")]
    Done,
    /// An in-progress run was detected for this conversation. The GUI should
    /// prompt the user to choose: resume the old plan, edit-and-resume, or
    /// abandon it and start fresh.
    #[serde(rename = "interrupt_prompt")]
    InterruptPrompt {
        run_id: String,
        goal: String,
        new_message: String,
    },
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
        .with_message_id(message_key)
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

    // Ensure stable cache_user_id for KVCache isolation (DeepSeek requires this
    // for prompt cache reuse across requests; without it, cache hit rate drops
    // to <1% because every request is treated as from a different user).
    //
    // Persisted to ~/.echo-agent/cache_user_id — generated once, reused forever.
    {
        let cache_id = echo_agent_app_core::infra::load_or_create_cache_user_id();
        agent_handle
            .write_async(|a| {
                Box::pin(async move {
                    a.config_mut().set_cache_user_id(&cache_id);
                })
            })
            .await;
    }

    // ── Interrupt detection ─────────────────────────────────────────────
    // If the same conversation already has an in-progress (Running/Paused)
    // run, do NOT start a new one. Instead, emit an InterruptPrompt event
    // so the GUI can ask the user what to do (resume / edit-and-resume /
    // abandon).
    if let Some(ref conv_id) = conversation_id
        && let Some(store) = state.app_state.tasks.runtime.as_ref()
        && let Ok(Some(existing)) = store.find_in_progress_run_by_conversation(conv_id)
    {
        emit_chat_event(
            &app,
            &ChatEvent::InterruptPrompt {
                run_id: existing.run_id.clone(),
                goal: existing.goal.clone(),
                new_message: message.clone(),
            },
            &message_key,
            &conversation_id,
        );
        return Ok(serde_json::json!({
            "kind": "interrupt_prompt",
            "run_id": existing.run_id,
        }));
    }

    // ── Complex-task router ────────────────────────────────────────────
    // Classify the input. If it looks like a complex, multi-step task, create
    // a TaskRuntime run and generate a structured plan instead of streaming a
    // normal chat. Missing conversation_id is handled by route_complex_task via
    // a message-scoped run id so Welcome-screen first turns still route.
    //
    let interaction_mode_raw = state
        .app_state
        .tasks
        .interaction_mode
        .load(Ordering::Relaxed);
    let permission_mode = state.app_state.config.permission_mode.read().await.clone();
    let execution_policy = ExecutionPolicy::from_raw(interaction_mode_raw, &permission_mode);

    if execution_policy.should_route_runtime() {
        let (route_llm, route_cache_user_id) = agent_handle
            .read(|a| {
                (
                    a.llm_client().cloned(),
                    a.config().get_cache_user_id().map(|s| s.to_string()),
                )
            })
            .await;
        let route_feedback = state.app_state.tasks.route_feedback.read().await.clone();
        let route_decision = echo_agent_app_core::tasks::task_runtime::route_message_with_feedback(
            route_llm,
            &message,
            execution_policy.interaction_mode,
            &route_feedback,
            route_cache_user_id.as_deref(),
        )
        .await;
        // ── Record route decision for long-term learning ──────────────
        {
            use echo_agent_app_core::tasks::task_runtime::{
                RouteDecisionRecord, append_route_record,
            };
            let record = RouteDecisionRecord {
                message_hash: compute_content_hash(&message),
                message_text: Some(message.clone()),
                route: route_decision.route,
                confidence: route_decision.confidence,
                matched_feedback_pattern: route_decision.matched_feedback_pattern.clone(),
                suggested_workers: route_decision.suggested_workers.clone(),
                actual_workers: None,
                final_run_status: None,
                user_correction: None,
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            let _ = append_route_record(&record);
        }
        if execution_policy.interaction_mode == InteractionMode::Auto
            && let Some(pattern) = route_decision.matched_feedback_pattern.as_deref()
        {
            let mut feedback = state.app_state.tasks.route_feedback.write().await;
            if echo_agent_app_core::tasks::task_runtime::record_route_feedback_pattern(
                pattern,
                &mut feedback,
            ) && let Err(error) =
                echo_agent_app_core::tasks::task_runtime::save_route_feedback_rules(&feedback)
            {
                tracing::warn!(%error, "failed to persist route feedback hit stats");
            }
        }
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
                execution_policy,
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
    let usage_store = state.app_state.tasks.runtime.clone();
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

        // ── Capture cache-diagnostic fingerprints before streaming ──
        let sys_prompt_hash = compute_content_hash(agent.config().get_system_prompt());
        let tools_hash = {
            let mut names: Vec<String> = agent.tool_names();
            names.sort();
            compute_content_hash(&names.join(","))
        };
        let cwd_hash = std::env::current_dir()
            .ok()
            .map(|p| compute_content_hash(&p.display().to_string()));
        // Infer provider from model name prefix (e.g. "deepseek-v4-pro" → "deepseek")
        let model_name = agent.config().get_model_name().to_string();
        let provider_name = model_name.split('-').next().map(|s| s.to_string());

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
                                                    system_prompt_hash: Some(
                                                        sys_prompt_hash.clone(),
                                                    ),
                                                    tools_schema_hash: Some(tools_hash.clone()),
                                                    cwd_hash: cwd_hash.clone(),
                                                    worker_prompt_hash: None,
                                                    provider: provider_name.clone(),
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
                                    // Persist usage to SQLite for trend analysis
                                    if let Some(ref store) = usage_store {
                                        let record =
                                            echo_agent_app_core::tasks::task_runtime::UsageRecord {
                                                id: uuid::Uuid::new_v4().to_string(),
                                                session_id: trace_session_id.clone(),
                                                run_id: None,
                                                worker_id: Some("main".to_string()),
                                                model: model.clone(),
                                                provider: provider_name.clone(),
                                                route_kind: Some("normal_chat".to_string()),
                                                input_tokens: prompt_tokens as u64,
                                                output_tokens: completion_tokens as u64,
                                                cached_input_tokens: cached_prompt_tokens as u64,
                                                cache_creation_input_tokens:
                                                    cache_creation_prompt_tokens as u64,
                                                usage_reported,
                                                system_prompt_hash: Some(sys_prompt_hash.clone()),
                                                tools_schema_hash: Some(tools_hash.clone()),
                                                cwd_hash: cwd_hash.clone(),
                                                worker_prompt_hash: None,
                                                created_at: chrono::Utc::now(),
                                            };
                                        let _ = store.insert_usage_record(&record);
                                    }
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
                                    if let Some(ref cid) = event_conversation_id {
                                        emit_conversation_event(
                                            &app_handle,
                                            &ConversationRuntimeEvent::FinalAnswer {
                                                content: data.clone(),
                                                usage_summary: None,
                                            },
                                            cid,
                                            usage_store.as_ref(),
                                        );
                                    }
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
                                    if let Some(ref cid) = event_conversation_id {
                                        emit_conversation_event(
                                            &app_handle,
                                            &ConversationRuntimeEvent::Error {
                                                stage: source.clone(),
                                                message: message.clone(),
                                                worker_id: None,
                                            },
                                            cid,
                                            usage_store.as_ref(),
                                        );
                                    }
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

/// Handle a complex input by creating a TaskRuntime run and dispatching it to
/// the unified launcher. All complex routes (ParallelReadonlyDelegation,
/// ComplexRuntime, PlanOnly) converge here.
///
/// The main agent runs a ReAct loop (with execute_plan tool for plan →
/// parallel execution), and events are streamed to chat://event.
async fn route_complex_task(
    state: &TauriState,
    app: tauri::AppHandle,
    message: String,
    conversation_id: Option<String>,
    message_key: String,
    execution_policy: ExecutionPolicy,
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

    // 1. Create the run in Pending.
    let run_id = uuid::Uuid::new_v4().to_string();
    store.create_run(
        &run_id,
        "default", // workspace_id — resolved properly in PR 6 workspace wiring
        &conv_id,
        "", // root_message_id — linked in PR 6
        route_decision.classification.inferred_profile,
        &message,
        route_decision.route.as_str(),
        AttendedMode::Attended,
    )?;

    // 2. Transition Pending → Running (valid in 6-state machine) and launch
    //    unified run. All routes share the same launcher — the route parameter
    //    controls execute_plan tool behavior (e.g. ComplexRuntime approval).
    store.transition_run(&run_id, TaskRunStatus::Running)?;
    let cancel = echo_agent::agent::CancellationToken::new();

    // G2 fix: Register the cancel token in session.cancel_token[message_key]
    // so that cancel_chat(message_key) — the command the GUI "stop" button
    // calls — can fire it. Without this, cancel_chat finds nothing for complex
    // runs (the token was only in run_cancel_tokens["__run__:{run_id}"], a
    // separate map), so subagents keep running after the user hits stop.
    // launch_unified_run derives a child_token from this parent, so firing
    // here propagates to the main agent stream + all workers.
    state
        .app_state
        .session
        .cancel_token
        .insert(message_key.clone(), cancel.clone());

    launch_unified_run(
        app.clone(),
        state,
        &run_id,
        &message_key,
        conversation_id.clone(),
        &message,
        route_decision.route,
        cancel,
    )
    .await?;

    // 3. Emit unified conversation event
    if let Some(ref cid) = conversation_id {
        use echo_agent_app_core::tasks::conversation_runtime::ConversationRuntimeEvent;
        let store_ref = state.app_state.tasks.runtime.as_ref();
        emit_conversation_event(
            &app,
            &ConversationRuntimeEvent::RouteDecision {
                route: route_decision.route.as_str().to_string(),
                confidence: route_decision.confidence,
                reason: route_decision.reason.clone(),
                matched_feedback_pattern: route_decision.matched_feedback_pattern.clone(),
                suggested_workers: route_decision.suggested_workers.clone(),
                interaction_mode: execution_policy.interaction_mode.as_str().to_string(),
            },
            cid,
            store_ref,
        );
    }

    tracing::info!(
        run_id = %run_id,
        route = ?route_decision.route,
        "task routed to unified run launcher"
    );

    Ok(serde_json::json!({
        "success": true,
        "run_id": run_id,
        "status": "running",
        "mode": "unified_run",
        "route": route_decision.route,
    }))
}

/// 统一启动器:所有复杂路由(ParallelReadonlyDelegation/ComplexRuntime/PlanOnly)
/// 都走这里。主 agent ReAct(pool 复用)+ execute_plan 工具(L1→L2)。
/// 替代 launch_main_agent_react + launch_task_run_execution(spec §3.1.2)。
#[allow(clippy::too_many_arguments)] // app/state/identity/conversation/routing/cancel all required
async fn launch_unified_run(
    app: tauri::AppHandle,
    state: &TauriState,
    run_id: &str,
    message_key: &str,
    conversation_id: Option<String>,
    goal: &str,
    route: TaskRouteKind,
    parent_cancel: CancellationToken,
) -> Result<(), anyhow::Error> {
    let primary_agent = state.app_state.connection.primary_agent();
    let child_cancel = parent_cancel.child_token();
    let run_key = format!("__run__:{run_id}");
    state
        .app_state
        .tasks
        .run_cancel_tokens
        .insert(run_key.clone(), child_cancel.clone());
    let run_cancel_tokens = state.app_state.tasks.run_cancel_tokens.clone();
    let store = state.app_state.tasks.runtime.clone();
    let trace_collector = state.app_state.trace.collector.clone();
    let usage_store = store.clone();
    let app_handle = app.clone();
    let run_id_owned = run_id.to_string();
    let message_key_owned = message_key.to_string();
    let goal_owned = goal.to_string();
    let event_message_key = message_key_owned.clone();
    let event_conversation_id = conversation_id.clone();
    let trace_session_id = conversation_id
        .clone()
        .unwrap_or_else(|| event_message_key.clone());

    // Read the cache_user_id from the primary agent config so it can be
    // scoped into task locals for execute_plan and the executor.
    let cache_user_id: String = primary_agent
        .read(|a| a.config().get_cache_user_id().map(|s| s.to_string()))
        .await
        .unwrap_or_else(|| {
            conversation_id
                .clone()
                .unwrap_or_else(|| run_id.to_string())
        });

    tokio::spawn(async move {
        let start = std::time::Instant::now();
        let mut terminal_status = "completed".to_string();

        // Construct a trace_sink that forwards WorkerTraceEvent to the frontend.
        // This reconnects the trace channel so execute_plan can send worker
        // trace events through the task-local mechanism (F1 fix).
        //
        // G1 fix: rewrite run_id for "main" worker events. chat_trace_event
        // hardcodes run_id=message_key, but the frontend filters workers by
        // activeRun.run_id (the TaskRuntime run_id). Without this rewrite,
        // the main agent's usage/token events are filtered out → the Token/Cache
        // card only shows subagent data. By rewriting main-agent events to
        // carry the TaskRuntime run_id, the frontend aggregator sees all agents.
        let app_for_sink = app_handle.clone();
        let msg_key_for_sink = event_message_key.clone();
        let run_id_for_sink = run_id_owned.clone();
        let trace_sink: Option<std::sync::Arc<dyn Fn(WorkerTraceEvent) + Send + Sync>> =
            Some(std::sync::Arc::new(move |mut event| {
                // Rewrite run_id for main-agent events so they land under the
                // TaskRuntime run_id (frontend filters by activeRun.run_id).
                if event.worker_id.as_deref() == Some("main") {
                    event.run_id = run_id_for_sink.clone();
                }
                if event.message_id.is_none() {
                    event.message_id = Some(msg_key_for_sink.clone());
                }
                let _ = emit_worker_trace_event(&app_for_sink, event);
            }));

        // 适配 trace_sink 为框架的 TraceSinkFn(Value-based),供主 agent 的
        // external context 使用(跨 spawn 安全,worker 经 ToolContext 读取)。
        // worker 的 emit_worker_trace 传 Value,这里反序列化成 WorkerTraceEvent
        // 再转发到原 sink(发前端)。
        let ext_trace_sink: Option<echo_core::tools::TraceSinkFn> = trace_sink.as_ref().map(|s| {
            let s = s.clone();
            std::sync::Arc::new(move |value: serde_json::Value| {
                if let Ok(ev) = serde_json::from_value::<WorkerTraceEvent>(value.clone()) {
                    s(ev);
                }
            }) as echo_core::tools::TraceSinkFn
        });

        // Set up task_local context so delegate_readonly + task_* tools work
        // (F1: also scope trace_sink and cache_user_id).
        let _ = echo_agent_app_core::tasks::task_runtime::task_tools::with_run_context(
            run_id_owned.clone(),
            child_cancel.clone(),
            trace_sink,
            cache_user_id.clone(),
            async {
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
                            "mode": "unified_run",
                            "run_id": run_id_owned,
                            "route": route.as_str(),
                        }),
                    ),
                );

                // Acquire agent and run ReAct
                let agent_inner = primary_agent.inner().clone();
                let agent = agent_inner.read().await;

                // 把 run context 注入主 agent(跨 spawn 安全的值传递)。
                // 主 agent 调 delegate_readonly/execute_plan 时,build_runtime_context
                // 从这些 external_* 读取并透传给 worker——绕开会跨 spawn 断裂的
                // task_local。worker 内的工具经 ToolContext 读到 run_id/cancel/...
                use echo_agent::agent::Agent;
                use echo_core::tools::ExternalRunContext;
                agent.set_external_context(&ExternalRunContext {
                    run_id: run_id_owned.clone(),
                    cancel: Some(std::sync::Arc::new(child_cancel.clone())),
                    trace_sink: ext_trace_sink.clone(),
                });

                let stream_result = agent
                    .execute_stream_with_cancel(&goal_owned, child_cancel.clone())
                    .await;

                match stream_result {
                    Ok(mut stream) => {
                        while let Some(event_result) = stream.next().await {
                            if child_cancel.is_cancelled() {
                                terminal_status = "cancelled".to_string();
                                break;
                            }
                            match event_result {
                                Ok(event) => {
                                    let chat_event = agent_event_to_chat_event(
                                        &app_handle,
                                        &event,
                                        &event_message_key,
                                        &event_conversation_id,
                                        &trace_session_id,
                                        &trace_collector,
                                        usage_store.as_ref(),
                                        &run_id_owned,
                                    );
                                    if let Some(ce) = chat_event
                                        && !emit_chat_event(
                                            &app_handle,
                                            &ce,
                                            &event_message_key,
                                            &event_conversation_id,
                                        )
                                    {
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
            },
        )
        .await;

        // Emit terminal status
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
                serde_json::json!({ "status": terminal_status.clone() }),
            ),
        );
        let _ = emit_chat_event(
            &app_handle,
            &ChatEvent::RunStatus {
                status: terminal_status.clone(),
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

        // Update run status in store.
        // 注意:execute_plan 工具内部调 execute_run 时,execute_run 可能已把 run
        // 转成终态(Completed/Failed,executor.rs:209/249)。若主 agent ReAct 结束
        // 吐 FinalAnswer 时 terminal_status 与 store 现状冲突(如 execute_plan
        // Failed 但 terminal_status="completed"),不要覆盖——保留 execute_run 设
        // 的更准确状态(它反映 plan 实际执行结果)。只在 store 还在 Running 时才转。
        if let Some(ref store) = store {
            let current_is_terminal = store
                .get_run(&run_id_owned)
                .ok()
                .flatten()
                .map(|r| {
                    matches!(
                        r.status,
                        TaskRunStatus::Completed | TaskRunStatus::Failed | TaskRunStatus::Cancelled
                    )
                })
                .unwrap_or(false);
            if !current_is_terminal {
                let new_status = match terminal_status.as_str() {
                    "completed" => TaskRunStatus::Completed,
                    "cancelled" => TaskRunStatus::Cancelled,
                    "failed" => TaskRunStatus::Failed,
                    _ => TaskRunStatus::Completed,
                };
                if let Err(e) = store.transition_run(&run_id_owned, new_status) {
                    tracing::error!(error = %e, run_id = %run_id_owned, "终态 transition 失败");
                }
            } else {
                tracing::info!(
                    run_id = %run_id_owned,
                    terminal_status = %terminal_status,
                    "run 已是终态(execute_plan 设的),保留不覆盖"
                );
            }
        }

        run_cancel_tokens.remove(&run_key);
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            run_id = %run_id_owned,
            status = %terminal_status,
            "unified_run finished"
        );
    });

    Ok(())
}

/// Map an AgentEvent to a ChatEvent, also emitting worker://trace side effects.
/// Returns None for events that should be silently ignored.
///
/// G1 fix: `run_id` is the TaskRuntime run_id. Worker trace events for the
/// main agent carry this run_id (not message_key) so the frontend aggregator
/// (which filters by activeRun.run_id) sees the main agent's token/usage data.
#[allow(clippy::too_many_arguments)] // event mapping requires full context
fn agent_event_to_chat_event(
    app: &tauri::AppHandle,
    event: &AgentEvent,
    message_key: &str,
    conversation_id: &Option<String>,
    _trace_session_id: &str,
    _trace_collector: &std::sync::Arc<echo_agent_app_core::observability::TraceCollector>,
    _usage_store: Option<
        &std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
    >,
    run_id: &str,
) -> Option<ChatEvent> {
    // Local helper: like chat_trace_event but uses the TaskRuntime run_id
    // so main-agent events land under the correct run in the frontend.
    let trace = |event_type, payload| {
        emit_worker_trace_event(
            app,
            WorkerTraceEvent::for_worker(run_id, "main", event_type, payload)
                .with_agent("echo-assistant")
                .with_title("Assistant")
                .with_message_id(message_key),
        )
    };
    match event {
        AgentEvent::Token(data) => {
            trace(
                WorkerTraceEventKind::WorkerTokenDelta,
                serde_json::json!({ "content": data }),
            );
            Some(ChatEvent::Token { data: data.clone() })
        }
        AgentEvent::ThinkStart => {
            let _ = emit_chat_event(
                app,
                &ChatEvent::RunStatus {
                    status: "thinking".to_string(),
                },
                message_key,
                conversation_id,
            );
            trace(
                WorkerTraceEventKind::WorkerThinkingStart,
                serde_json::json!({}),
            );
            Some(ChatEvent::ThinkingStart)
        }
        AgentEvent::ThinkEnd {
            prompt_tokens,
            completion_tokens,
        } => {
            trace(
                WorkerTraceEventKind::WorkerThinkingEnd,
                serde_json::json!({
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens
                }),
            );
            Some(ChatEvent::ThinkingEnd {
                prompt_tokens: *prompt_tokens,
                completion_tokens: *completion_tokens,
            })
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
            trace(
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
            );
            Some(ChatEvent::LlmUsage {
                model: model.clone(),
                prompt_tokens: *prompt_tokens,
                completion_tokens: *completion_tokens,
                total_tokens: *total_tokens,
                cached_prompt_tokens: *cached_prompt_tokens,
                cache_creation_prompt_tokens: *cache_creation_prompt_tokens,
                usage_reported: *usage_reported,
            })
        }
        AgentEvent::ToolCall { name, args } => {
            let _ = emit_chat_event(
                app,
                &ChatEvent::RunStatus {
                    status: "using_tool".to_string(),
                },
                message_key,
                conversation_id,
            );
            trace(
                WorkerTraceEventKind::WorkerToolStart,
                serde_json::json!({ "name": name, "args": args }),
            );
            Some(ChatEvent::ToolStart {
                name: name.clone(),
                args: args.clone(),
            })
        }
        AgentEvent::ToolResult { name, output } => {
            trace(
                WorkerTraceEventKind::WorkerToolResult,
                serde_json::json!({
                    "name": name,
                    "result": output,
                    "success": true
                }),
            );
            Some(ChatEvent::ToolResult {
                name: name.clone(),
                result: output.clone(),
                success: true,
            })
        }
        AgentEvent::ToolError { name, error } => {
            trace(
                WorkerTraceEventKind::WorkerToolResult,
                serde_json::json!({
                    "name": name,
                    "result": error,
                    "success": false
                }),
            );
            Some(ChatEvent::ToolResult {
                name: name.clone(),
                result: error.clone(),
                success: false,
            })
        }
        AgentEvent::ToolBatchStart { tool_count } => Some(ChatEvent::ToolBatchStart {
            tool_count: *tool_count,
        }),
        AgentEvent::ToolBatchEnd => Some(ChatEvent::ToolBatchEnd),
        AgentEvent::Chart { spec } => Some(ChatEvent::Chart { spec: spec.clone() }),
        AgentEvent::FinalAnswer(data) => {
            emit_worker_trace_event(
                app,
                chat_trace_event(
                    message_key,
                    WorkerTraceEventKind::WorkerCompleted,
                    serde_json::json!({}),
                ),
            );
            Some(ChatEvent::FinalAnswer { data: data.clone() })
        }
        AgentEvent::Cancelled => {
            emit_worker_trace_event(
                app,
                chat_trace_event(
                    message_key,
                    WorkerTraceEventKind::WorkerCancelled,
                    serde_json::json!({}),
                ),
            );
            Some(ChatEvent::Cancelled)
        }
        AgentEvent::Error { source, message } => {
            emit_worker_trace_event(
                app,
                chat_trace_event(
                    message_key,
                    WorkerTraceEventKind::WorkerFailed,
                    serde_json::json!({
                        "source": source,
                        "message": message
                    }),
                ),
            );
            Some(ChatEvent::Error {
                message: format!("{source}: {message}"),
            })
        }
        _ => None,
    }
}

/// Compute cache diagnostics for a session or across all sessions.
#[tauri::command]
pub async fn get_cache_diagnostics(
    state: tauri::State<'_, TauriState>,
    session_id: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    use echo_agent_app_core::observability::compute_cache_diagnostics;

    let collector = &state.app_state.trace.collector;
    let mut events = if let Some(sid) = &session_id {
        collector.get_events(sid).await
    } else {
        let sessions = collector.list_sessions().await;
        let mut all = Vec::new();
        for sid in &sessions {
            all.extend(collector.get_events(sid).await);
        }
        all
    };

    // If the in-memory trace is empty (e.g. after restart), fall back to
    // persisted usage records from the last 24h.
    if events.is_empty()
        && let Some(ref store) = state.app_state.tasks.runtime
        && let Ok(records) = store.query_usage_records(
            &echo_agent_app_core::tasks::task_runtime::UsageQueryFilter {
                limit: Some(200),
                ..Default::default()
            },
        )
    {
        for r in &records {
            events.push(TraceEvent {
                timestamp: r.created_at,
                kind: TraceKind::LlmCall {
                    model: r.model.clone(),
                    input_tokens: r.input_tokens,
                    output_tokens: r.output_tokens,
                    cached_input_tokens: r.cached_input_tokens,
                    cache_creation_input_tokens: r.cache_creation_input_tokens,
                    usage_reported: r.usage_reported,
                    system_prompt_hash: r.system_prompt_hash.clone(),
                    tools_schema_hash: r.tools_schema_hash.clone(),
                    cwd_hash: r.cwd_hash.clone(),
                    worker_prompt_hash: r.worker_prompt_hash.clone(),
                    provider: r.provider.clone(),
                },
                duration_ms: None,
                metadata: std::collections::HashMap::new(),
            });
        }
    }

    let diagnostics = compute_cache_diagnostics(&events);
    let recent_calls: Vec<serde_json::Value> = events
        .iter()
        .rev()
        .filter_map(|e| match &e.kind {
            TraceKind::LlmCall {
                model,
                input_tokens,
                cached_input_tokens: cached,
                system_prompt_hash,
                tools_schema_hash,
                cwd_hash,
                worker_prompt_hash,
                provider,
                ..
            } => Some(serde_json::json!({
                "model": model,
                "input_tokens": input_tokens,
                "cached_input_tokens": cached,
                "system_prompt_hash": system_prompt_hash,
                "tools_schema_hash": tools_schema_hash,
                "cwd_hash": cwd_hash,
                "worker_prompt_hash": worker_prompt_hash,
                "provider": provider,
            })),
            _ => None,
        })
        .take(20)
        .collect();

    Ok(serde_json::json!({
        "overall_read_rate": diagnostics.overall_read_rate,
        "total_input_tokens": diagnostics.total_input_tokens,
        "total_cached_input_tokens": diagnostics.total_cached_input_tokens,
        "total_cache_creation_input_tokens": diagnostics.total_cache_creation_input_tokens,
        "total_llm_calls": diagnostics.total_llm_calls,
        "calls_missing_usage": diagnostics.calls_missing_usage,
        "distinct_models": diagnostics.distinct_models,
        "issues": diagnostics.issues.iter().map(|i| serde_json::json!({
            "kind": i.kind,
            "severity": i.severity,
            "message": i.message,
            "affected_calls": i.affected_calls,
        })).collect::<Vec<_>>(),
        "suggested_fixes": diagnostics.suggested_fixes,
        "recent_calls": recent_calls,
    }))
}
