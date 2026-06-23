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
    ExecutionPolicy, InteractionMode, TaskPlan, TaskRouteDecision, TaskRunStatus, WorkerTraceEvent,
    WorkerTraceEventKind,
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
    if let Some(ref store) = store {
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
        let (llm, plan_cache_user_id) = state
            .app_state
            .connection
            .primary_agent()
            .read(|a| {
                (
                    a.llm_client().cloned(),
                    a.config().get_cache_user_id().map(|s| s.to_string()),
                )
            })
            .await;
        let llm = llm.ok_or_else(|| anyhow::anyhow!("no LLM client available on primary agent"))?;
        let cache_user_id = plan_cache_user_id.unwrap_or_default();
        echo_agent_app_core::tasks::task_runtime::generate_plan(
            &llm,
            &run_id,
            &message,
            &route_decision.classification,
            &route_decision.suggested_workers,
            &cache_user_id,
        )
        .await?
    };

    // 4. Persist + advance to AwaitingPlanApproval (attach_plan is atomic).
    store.attach_plan(&generated.plan)?;
    let planned_workers = planned_worker_roles(&generated.plan);
    let all_plan_tasks_read_only = generated
        .plan
        .tasks
        .iter()
        .all(|task| task.kind.is_read_only());
    let launch_policy =
        execution_policy.runtime_launch_policy(route_decision.route, all_plan_tasks_read_only);
    let mut response_status = TaskRunStatus::AwaitingPlanApproval;
    if launch_policy.auto_execute {
        store.transition_run(&run_id, TaskRunStatus::Ready)?;
        launch_task_run_execution(
            state,
            app.clone(),
            &run_id,
            Some(compute_content_hash(&message)),
        )
        .await?;
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
            interaction_mode: execution_policy.interaction_mode.as_str().to_string(),
            permission_mode: execution_policy.permission_mode.as_str().to_string(),
            approval_policy: launch_policy.approval_policy.clone(),
            route_reason: route_decision.reason.clone(),
            confidence: route_decision.confidence,
            auto_execute: launch_policy.auto_execute,
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

    // Emit unified conversation event
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
        plan_id = %generated.plan.plan_id,
        task_count = generated.plan.tasks.len(),
        route = ?route_decision.route,
        auto_execute = launch_policy.auto_execute,
        "task routed to TaskRuntime"
    );

    Ok(serde_json::json!({
        "kind": "complex_task",
        "run_id": run_id,
        "status": response_status.as_str(),
        "route": route_decision.route,
        "auto_execute": launch_policy.auto_execute,
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

async fn launch_task_run_execution(
    state: &TauriState,
    app: tauri::AppHandle,
    run_id: &str,
    message_hash: Option<String>,
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
    let (reviewer_llm, exec_cache_user_id) = primary_agent
        .read(|a| {
            (
                a.llm_client().cloned(),
                a.config().get_cache_user_id().map(|s| s.to_string()),
            )
        })
        .await;
    let exec_cache_user_id = exec_cache_user_id.unwrap_or_default();
    let layer_manager = state
        .app_state
        .review_integration
        .as_ref()
        .map(|ri| std::sync::Arc::new(ri.create_layer_manager()));
    let cancel = echo_agent::agent::CancellationToken::new();
    let run_id_for_task = run_id.to_string();
    let message_hash_for_task = message_hash.clone();
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
            exec_cache_user_id.clone(),
            cancel,
        )
        .await;
        run_cancel_tokens.remove(&format!("__run__:{run_id_for_task}"));
        let final_status = match &outcome {
            Ok(echo_agent_app_core::tasks::task_runtime::RunOutcome::Completed) => {
                tracing::info!(run_id = %run_id_for_task, "auto-routed run completed");
                Some("completed".to_string())
            }
            Ok(other) => {
                tracing::warn!(run_id = %run_id_for_task, ?other, "auto-routed run ended non-completed");
                Some(format!("{:?}", other))
            }
            Err(e) => {
                tracing::error!(run_id = %run_id_for_task, error = %e, "auto-routed run executor error");
                Some("failed".to_string())
            }
        };
        // Update route decision record with run outcome
        if let Some(ref hash) = message_hash_for_task {
            use echo_agent_app_core::tasks::task_runtime::load_route_records;
            let mut records = load_route_records();
            let actual_workers = store_for_task
                .get_plan(&run_id_for_task)
                .ok()
                .flatten()
                .map(|plan| planned_worker_roles(&plan));
            // Update the latest matching record
            for record in records.iter_mut().rev() {
                if record.message_hash == *hash && record.final_run_status.is_none() {
                    record.final_run_status = final_status.clone();
                    record.actual_workers = actual_workers.clone();
                    break;
                }
            }
            // Re-persist all records (simplified: rewrite)
            let path = echo_agent_app_core::tasks::task_runtime::default_route_records_path();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let content = records
                .iter()
                .map(|r| serde_json::to_string(r).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            let _ = std::fs::write(&path, content);
        }
    });

    Ok(())
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
    if events.is_empty() {
        if let Some(ref store) = state.app_state.tasks.runtime {
            if let Ok(records) = store.query_usage_records(
                &echo_agent_app_core::tasks::task_runtime::UsageQueryFilter {
                    limit: Some(200),
                    ..Default::default()
                },
            ) {
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
