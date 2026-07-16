//! Tauri IPC commands for chat streaming.
//!
//! Uses `app.emit()` to stream `AgentEvent` items to the frontend,
//! replacing the WebSocket transport from the Axum server.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use chrono::Utc;
use echo_agent::agent::CancellationToken;
use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use echo_agent::prelude::AgentEvent;
use echo_agent::tools::{ToolOutputChannel, ToolStreamEvent};
use echo_agent_app_core::chat_driver::ChatSink;
use echo_agent_app_core::observability::{TraceEvent, TraceKind};
use echo_agent_app_core::tasks::task_runtime::executor::ExecEvent;
use futures::future::BoxFuture;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
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
    /// Auto-compact 后通知前端：Snapshot 置空，Accumulator 保留。
    #[serde(rename = "context_compressed")]
    ContextCompressed {
        before_count: usize,
        after_count: usize,
        before_tokens: usize,
        after_tokens: usize,
    },
    #[serde(rename = "tool_start")]
    ToolStart {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    #[serde(rename = "tool_progress")]
    ToolProgress {
        call_id: String,
        message: String,
        percent: Option<u8>,
    },
    #[serde(rename = "tool_output")]
    ToolOutput {
        call_id: String,
        channel: String,
        chunk: String,
    },
    #[serde(rename = "tool_complete")]
    ToolComplete {
        call_id: String,
        success: bool,
        metadata: std::collections::HashMap<String, String>,
        truncated: bool,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        call_id: String,
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

/// Emit an event on the unified `execution://event` channel. `kind` is either
/// "run" (TaskRun lifecycle: RunStarted/Completed/Failed/Cancelled/StatusChanged)
/// or "subagent" (task-scoped execution flow: Thinking/Tool/Token/Usage).
///
/// `subagent_run_id` is the aggregation key the frontend store uses to group a
/// subagent's events into one card. It MUST match the framework bridge
/// (`src/tauri/mod.rs`), which emits the bare `task_id` (NOT `"{task_id}:{attempt}"`
/// — the attempt suffix was dropped so retry attempts fold into one card, matching
/// how Claude Code/Codex display subagents). For `kind == "run"` (run-level
/// events with no owning task), pass `""`; this function only attaches the field
/// for `kind == "subagent"`.
fn emit_execution_event(
    app: &tauri::AppHandle,
    run_id: &str,
    kind: &str,
    event: &str,
    agent: &str,
    subagent_run_id: &str,
    payload: serde_json::Value,
) {
    let mut map = serde_json::Map::new();
    map.insert("kind".into(), kind.into());
    if kind == "subagent" {
        // Fall back to "main" only when a caller genuinely has no task_id
        // (shouldn't happen for kind="subagent", but guards against empty string).
        let id = if subagent_run_id.is_empty() {
            "main"
        } else {
            subagent_run_id
        };
        map.insert("subagent_run_id".into(), id.into());
        map.insert("agent".into(), agent.into());
    }
    map.insert("run_id".into(), run_id.into());
    map.insert("event".into(), event.into());
    if let serde_json::Value::Object(fields) = payload {
        for (k, v) in fields {
            map.insert(k, v);
        }
    }
    let _ = app.emit("execution://event", serde_json::Value::Object(map));
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
    attachments: Option<Vec<echo_agent_app_core::types::AttachmentData>>,
) -> Result<serde_json::Value, IpcError> {
    // ── Persist attachments + build multimodal message (if any) ──────────
    // The frontend base64-encodes uploads; we write them to a per-workspace
    // uploads dir and rebuild a `Message` with the right ContentParts so the
    // LLM sees images/files. When there are no attachments we keep the plain
    // `&str` path unchanged for zero overhead.
    let saved_attachments = attachments.unwrap_or_default();
    let (multimodal_message, attachment_refs): (
        Option<echo_core::llm::types::Message>,
        Vec<echo_agent_app_core::attachments::AttachmentRef>,
    ) = if saved_attachments.is_empty() {
        (None, Vec::new())
    } else {
        let ws_root = state.app_state.current_workspace().await.map(|ws| ws.root);
        let uploads_dir = echo_agent_app_core::attachments::resolve_uploads_dir(ws_root.as_deref());
        let saved =
            echo_agent_app_core::attachments::save_attachments(&saved_attachments, &uploads_dir);
        // Build refs (path + name + mime) for binding to the run so plan-level
        // subagents can rebuild the multimodal message later.
        let refs: Vec<_> = saved
            .iter()
            .map(|(path, att)| {
                echo_agent_app_core::attachments::AttachmentRef::from_saved(path.clone(), att)
            })
            .collect();
        let msg = if saved.is_empty() {
            None
        } else {
            match echo_agent_app_core::attachments::build_message(&message, &saved) {
                Ok(msg) => Some(msg),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to build multimodal message, sending text only");
                    None
                }
            }
        };
        (msg, refs)
    };
    let has_attachments = multimodal_message.is_some();
    if has_attachments {
        tracing::info!(
            count = saved_attachments.len(),
            "send_chat_message: multimodal message with attachments"
        );
    }

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

    let active_turn_key = conversation_id
        .clone()
        .unwrap_or_else(|| format!("message:{message_key}"));
    match state
        .app_state
        .session
        .active_chat_turns
        .entry(active_turn_key.clone())
    {
        dashmap::mapref::entry::Entry::Occupied(entry) => {
            return Err(IpcError::Validation(format!(
                "chat_turn_busy:{}",
                entry.get()
            )));
        }
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            entry.insert(message_key.clone());
        }
    }

    let cancel_token = CancellationToken::new();

    // Register cancel token so `cancel_chat(message_key)` (the GUI "stop"
    // button) can fire it for this chat turn. Background runs created by the
    // agent via `create_complex_task` use an INDEPENDENT token (spec §5.5) —
    // firing this one cancels the inline chat reply, not the background run.
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
    let browser_approval_key = conversation_id
        .clone()
        .unwrap_or_else(|| "browser-default".to_string());
    state
        .browser_runtime
        .set_conversation_approval_provider(browser_approval_key.clone(), hitl_handler.clone())
        .await;

    // Capture cache-diagnostic fingerprints BEFORE streaming (same as the
    // pre-B4 inline normal path): they ride along in the sink so the unified
    // `agent_event_to_chat_event` records usage/trace with cache diagnostics
    // (B4.1 — fixes the drift where complex runs dropped observability).
    let trace_collector = state.app_state.trace.collector.clone();
    let usage_store = state.app_state.tasks.runtime.clone();
    let trace_session_id = conversation_id
        .clone()
        .unwrap_or_else(|| message_key.clone());
    let (sys_prompt_hash, tools_hash, cwd_hash, provider_name) = agent_handle
        .read(|agent| {
            let sys_prompt_hash = compute_content_hash(agent.config().get_system_prompt());
            let mut tool_names: Vec<String> = agent.tool_names();
            tool_names.sort();
            let tools_hash = compute_content_hash(&tool_names.join(","));
            let cwd_hash = std::env::current_dir()
                .ok()
                .map(|p| compute_content_hash(&p.display().to_string()));
            let model_name = agent.config().get_model_name().to_string();
            let provider_name = model_name.split('-').next().map(|s| s.to_string());
            (sys_prompt_hash, tools_hash, cwd_hash, provider_name)
        })
        .await;

    use echo_agent_app_core::tasks::task_runtime::InteractionMode;
    let raw = state
        .app_state
        .tasks
        .interaction_mode
        .load(std::sync::atomic::Ordering::Relaxed);
    let interaction_mode = InteractionMode::from_u8(raw);
    let mode_hint = Some(interaction_mode.prompt_hint().to_string());

    // Build the GUI sink + per-turn resources, then drive the whole turn
    // (normal reply AND any complex runs the agent autonomously spins up via
    // create_complex_task) through the single shared `drive_chat` entry. The
    // agent decides complexity itself (Phase B3) — no code route pre-judgment.
    let sink = std::sync::Arc::new(TauriChatSink {
        app: app.clone(),
        message_key: message_key.clone(),
        conversation_id: conversation_id.clone(),
        trace_session_id: trace_session_id.clone(),
        trace_collector: trace_collector.clone(),
        usage_store: usage_store.clone(),
        run_id: message_key.clone(),
        route: format!("requested:{}", interaction_mode.as_str()),
        sys_prompt_hash,
        tools_hash,
        cwd_hash,
        provider_name,
    });
    // Signal the chat-turn lifecycle so the GUI shows the spinner / terminal
    // badge. Ordinary chat turns are not TaskRuntime runs.
    sink.on_turn_status("running");

    let res = std::sync::Arc::new(echo_agent_app_core::chat_resources::ChatResources {
        pool: state.app_state.connection.pool.clone(),
        store: state.app_state.tasks.runtime.clone(),
        sink: sink.clone(),
        conv_id: conversation_id.clone(),
        root_message_id: message_key.clone(),
        attachments: attachment_refs.clone(),
        cancel: cancel_token.clone(),
        mode_hint,
        interaction_mode,
        // B5.1: wire the memory layer so create_complex_task's autonomous runs
        // block-write their completion memory (recall closure). None when the
        // review/memory subsystem isn't initialized (write becomes a no-op).
        layer_manager: state
            .app_state
            .review_integration
            .as_ref()
            .map(|ri| std::sync::Arc::new(ri.create_layer_manager())),
    });

    let agent_handle_clone = agent_handle.clone();
    let cleanup_tokens = state.app_state.session.cancel_token.clone();
    let cleanup_key = message_key.clone();
    let active_chat_turns = state.app_state.session.active_chat_turns.clone();
    let active_turn_key_for_cleanup = active_turn_key.clone();
    let cancel_for_status = cancel_token.clone();
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        // Multimodal: drive_chat takes Option<&Message>; pass the pre-built one
        // (images/files) so the agent sees attachments this turn. Background
        // runs created by create_complex_task pick up attachments via
        // ChatResources.attachments (already bound above).
        let multimodal_ref = multimodal_message.as_ref();
        let outcome = echo_agent_app_core::chat_driver::drive_chat(
            &agent_handle_clone,
            &message,
            multimodal_ref,
            res,
        )
        .await;
        let terminal_status = if cancel_for_status.is_cancelled() {
            "cancelled"
        } else if outcome.is_ok() {
            "completed"
        } else {
            "failed"
        };
        if let Err(e) = &outcome {
            tracing::warn!(error = %e, "drive_chat chat turn errored");
        }
        cleanup_tokens.remove(&cleanup_key);
        if active_chat_turns
            .get(&active_turn_key_for_cleanup)
            .is_some_and(|entry| entry.value() == &cleanup_key)
        {
            active_chat_turns.remove(&active_turn_key_for_cleanup);
        }
        // Release all execution ownership before emitting Done. The frontend
        // may immediately dispatch the next queued turn when it receives it.
        sink.on_turn_status(terminal_status);
        agent_handle_clone
            .write_async(|agent| {
                Box::pin(async move {
                    let empty = Arc::new(echo_agent_app_core::hitl::HitlDispatcher::new());
                    agent.set_human_loop_provider_preserving_approvals(empty);
                })
            })
            .await;
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            status = %terminal_status,
            "Tauri chat turn finished (drive_chat)"
        );
    });

    Ok(serde_json::json!({
        "success": true,
        "message_key": message_key,
    }))
}

/// Inject additional user input into the active foreground turn.
#[tauri::command]
pub async fn steer_chat_message(
    state: tauri::State<'_, TauriState>,
    message: String,
    conversation_id: String,
    attachments: Option<Vec<echo_agent_app_core::types::AttachmentData>>,
) -> Result<serde_json::Value, IpcError> {
    if message.trim().is_empty() && attachments.as_ref().is_none_or(Vec::is_empty) {
        return Err(IpcError::Validation("steer input is empty".to_string()));
    }
    let expected_turn_id = state
        .app_state
        .session
        .active_chat_turns
        .get(&conversation_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| IpcError::Validation("no active chat turn".to_string()))?;
    let saved_attachments = attachments.unwrap_or_default();
    let steer_message = if saved_attachments.is_empty() {
        echo_core::llm::types::Message::user(message)
    } else {
        let ws_root = state.app_state.current_workspace().await.map(|ws| ws.root);
        let uploads_dir = echo_agent_app_core::attachments::resolve_uploads_dir(ws_root.as_deref());
        let saved =
            echo_agent_app_core::attachments::save_attachments(&saved_attachments, &uploads_dir);
        if saved.is_empty() {
            echo_core::llm::types::Message::user(message)
        } else {
            echo_agent_app_core::attachments::build_message(&message, &saved)
                .map_err(|error| IpcError::Validation(error.to_string()))?
        }
    };
    let agent = state.app_state.connection.agent_for(&conversation_id).await;
    match agent
        .steer_input(Some(&expected_turn_id), steer_message)
        .await
    {
        Ok(turn_id) => Ok(serde_json::json!({
            "kind": "accepted",
            "turn_id": turn_id,
        })),
        Err(echo_agent::agent::TurnSteerError::NotSteerable { turn_id }) => Ok(serde_json::json!({
            "kind": "not_steerable",
            "turn_id": turn_id,
        })),
        Err(echo_agent::agent::TurnSteerError::NoActiveTurn) => {
            Ok(serde_json::json!({"kind": "no_active_turn"}))
        }
        Err(echo_agent::agent::TurnSteerError::TurnMismatch { expected, actual }) => {
            Ok(serde_json::json!({
                "kind": "turn_mismatch",
                "expected": expected,
                "actual": actual,
            }))
        }
        Err(error) => Err(IpcError::Validation(error.to_string())),
    }
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

/// GUI `ChatSink`: bridges the shared `drive_chat` stream to the Tauri frontend
/// by emitting `ChatEvent`s + subagent trace events.
///
/// This is the GUI equivalent of the TUI/channel `ChatSink`: the whole chat
/// turn (normal reply + any complex runs the agent autonomously spins up via
/// `create_complex_task`) flows through one unified `drive_chat`, and this sink
/// turns each `AgentEvent` into the exact GUI emit sequence (`agent_event_to_chat_event`).
struct TauriChatSink {
    app: tauri::AppHandle,
    message_key: String,
    conversation_id: Option<String>,
    trace_session_id: String,
    trace_collector: std::sync::Arc<echo_agent_app_core::observability::TraceCollector>,
    usage_store: Option<std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    run_id: String,
    route: String,
    // Cache-diagnostic fingerprints captured before streaming starts, so the
    // unified `agent_event_to_chat_event` can record usage/trace exactly like
    // the (now-removed) inline normal-chat match did (B4.1/B4.2 — fixes the
    // drift where complex runs dropped usage/trace observability).
    sys_prompt_hash: String,
    tools_hash: String,
    cwd_hash: Option<String>,
    provider_name: Option<String>,
}

impl echo_agent_app_core::chat_driver::ChatSink for TauriChatSink {
    fn on_agent_event(&self, event: echo_agent::agent::EventEnvelope) -> bool {
        let chat_event = agent_event_to_chat_event(
            &self.app,
            &event.payload,
            &self.message_key,
            &self.conversation_id,
            &self.trace_session_id,
            &self.trace_collector,
            self.usage_store.as_ref(),
            &self.run_id,
            &self.sys_prompt_hash,
            &self.tools_hash,
            &self.cwd_hash,
            &self.provider_name,
            &self.route,
        );
        if let Some(ce) = chat_event {
            emit_chat_event(&self.app, &ce, &self.message_key, &self.conversation_id)
        } else {
            true
        }
    }

    fn on_turn_status(&self, status: &str) {
        // A chat turn is not a TaskRuntime run. Only emit chat transport state;
        // real run lifecycle events come from the TaskRuntime ExecSink.
        if status == "running" {
            emit_chat_event(
                &self.app,
                &ChatEvent::RunStatus {
                    status: "running".to_string(),
                },
                &self.message_key,
                &self.conversation_id,
            );
            return;
        }
        let _ = emit_chat_event(
            &self.app,
            &ChatEvent::RunStatus {
                status: status.to_string(),
            },
            &self.message_key,
            &self.conversation_id,
        );
        let _ = emit_chat_event(
            &self.app,
            &ChatEvent::Done,
            &self.message_key,
            &self.conversation_id,
        );
    }

    fn on_execution_path(&self, requested_mode: &str, observed_path: &str) {
        let mut metadata = HashMap::new();
        metadata.insert(
            "requested_mode".to_string(),
            serde_json::Value::String(requested_mode.to_string()),
        );
        metadata.insert(
            "observed_path".to_string(),
            serde_json::Value::String(observed_path.to_string()),
        );
        self.trace_collector.record_sync(
            &self.trace_session_id,
            TraceEvent {
                timestamp: Utc::now(),
                kind: TraceKind::PipelineStage {
                    pipeline: "agent_route".to_string(),
                    stage: observed_path.to_string(),
                },
                duration_ms: None,
                metadata,
            },
        );
    }

    fn worker_trace_sink(&self) -> Option<crate::tasks::task_runtime::task_tools::TraceSink> {
        // Forward execution-flow events from `execute_run` (run lifecycle +
        // main-agent task stream) to the unified `execution://event` channel.
        // Run-level events (task_id None) → kind="run"; task-level events
        // (task_id Some) → kind="subagent" keyed by the task_id (NOT a hardcoded
        // "main"), so each subagent's lifecycle events aggregate with its own
        // thinking/tool/usage stream (which the framework bridge emits under
        // the same bare task_id). The old code hardcoded "main" here, which
        // collided all subagents into one store record and broke the todo join.
        // Note: the normal chat-turn thinking/tool stream does NOT go through
        // this sink — it goes through `chat://event` via agent_event_to_chat_event.
        // This sink only fires for events emitted by `execute_run` /
        // `run_main_agent_task` (verification tasks run on the primary agent).
        let app = self.app.clone();
        let run_id = self.run_id.clone();
        Some(std::sync::Arc::new(move |ev: ExecEvent| {
            let task_id = ev.task_id.clone();
            let agent = ev.agent.as_deref().unwrap_or("echo-assistant");
            // subagent_run_id = bare task_id (matches framework bridge);
            // "main" only for genuine run-level events (task_id None).
            let subagent_run_id = task_id.as_deref().unwrap_or("main");
            let kind = if task_id.is_some() { "subagent" } else { "run" };
            let mut payload = ev.payload;
            if let (Some(task_id), serde_json::Value::Object(fields)) = (&task_id, &mut payload) {
                fields.insert("task_id".into(), task_id.clone().into());
            }
            emit_execution_event(
                &app,
                &run_id,
                kind,
                &ev.event,
                agent,
                subagent_run_id,
                payload,
            );
        }))
    }

    fn trace_sink(&self) -> Option<echo_core::tools::TraceSinkFn> {
        // Bridge the framework's Value-based trace_sink to worker_trace_sink
        // so tools running inside a spawned task executor (e.g. plan_execute)
        // can reach CURRENT_TRACE_SINK via scoped_with_ctx_run_id.
        let ws = self.worker_trace_sink()?;
        Some(std::sync::Arc::new(move |value: serde_json::Value| {
            if let Ok(ev) = serde_json::from_value::<ExecEvent>(value) {
                ws(ev);
            }
        }) as echo_core::tools::TraceSinkFn)
    }
}

/// Map an AgentEvent to a ChatEvent, also emitting execution trace side effects.
/// Returns None for events that should be silently ignored.
///
/// G1 fix: `run_id` is the TaskRuntime run_id. Subagent execution events for the
/// main agent carry this run_id (not message_key) so the frontend aggregator
/// (which filters by activeRun.run_id) sees the main agent's token/usage data.
///
/// B4.1: this unified mapper now ALSO records usage/trace (the work the
/// removed inline normal-chat match used to do), so normal AND complex runs
/// keep observability parity. The fingerprints (`sys_prompt_hash`/`tools_hash`
/// /`cwd_hash`/`provider_name`) come from the `TauriChatSink`, captured before
/// streaming started.
#[allow(clippy::too_many_arguments)] // event mapping requires full context
fn agent_event_to_chat_event(
    app: &tauri::AppHandle,
    event: &AgentEvent,
    message_key: &str,
    conversation_id: &Option<String>,
    trace_session_id: &str,
    trace_collector: &std::sync::Arc<echo_agent_app_core::observability::TraceCollector>,
    usage_store: Option<
        &std::sync::Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
    >,
    run_id: &str,
    sys_prompt_hash: &str,
    tools_hash: &str,
    cwd_hash: &Option<String>,
    provider_name: &Option<String>,
    route: &str,
) -> Option<ChatEvent> {
    // Local helper removed (Phase 4c follow-up): the main agent's execution
    // flow is already rendered via `chat://event` (ChatPanel). Emitting the
    // same events onto `execution://event` (kind="subagent", id="main") caused
    // duplicate rendering in SubagentStreamBlock AND a stale "running" card
    // (main run had no `started`/`completed` lifecycle pairing). Main-agent
    // cache diagnostics go through `trace_collector` + the file-backed runtime store via
    // `get_cache_diagnostics`, not through the execution://event store.
    match event {
        AgentEvent::Token(data) => Some(ChatEvent::Token { data: data.clone() }),
        AgentEvent::ThinkStart => {
            let _ = emit_chat_event(
                app,
                &ChatEvent::RunStatus {
                    status: "thinking".to_string(),
                },
                message_key,
                conversation_id,
            );
            Some(ChatEvent::ThinkingStart)
        }
        AgentEvent::ThinkEnd {
            prompt_tokens,
            completion_tokens,
        } => Some(ChatEvent::ThinkingEnd {
            prompt_tokens: *prompt_tokens,
            completion_tokens: *completion_tokens,
        }),
        AgentEvent::LlmUsage {
            model,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_prompt_tokens,
            cache_creation_prompt_tokens,
            usage_reported,
        } => {
            // B4.1: record usage/trace exactly like the removed inline
            // normal-chat match did, so normal AND complex runs keep cache
            // diagnostics. Ported verbatim (field-for-field) from the pre-B4
            // inline `LlmUsage` arm. Uses `record_sync` because this mapper
            // runs inside the sync `ChatSink::on_agent_event`.
            trace_collector.record_sync(
                trace_session_id,
                TraceEvent {
                    timestamp: Utc::now(),
                    kind: TraceKind::LlmCall {
                        model: model.clone(),
                        input_tokens: *prompt_tokens as u64,
                        output_tokens: *completion_tokens as u64,
                        cached_input_tokens: *cached_prompt_tokens as u64,
                        cache_creation_input_tokens: *cache_creation_prompt_tokens as u64,
                        usage_reported: *usage_reported,
                        system_prompt_hash: Some(sys_prompt_hash.to_string()),
                        tools_schema_hash: Some(tools_hash.to_string()),
                        cwd_hash: cwd_hash.clone(),
                        worker_prompt_hash: None,
                        provider: provider_name.clone(),
                    },
                    duration_ms: None,
                    metadata: HashMap::from([
                        ("message_key".to_string(), serde_json::json!(message_key)),
                        ("total_tokens".to_string(), serde_json::json!(total_tokens)),
                    ]),
                },
            );
            // Persist usage to the local file-backed runtime store for trend analysis.
            if let Some(store) = usage_store {
                let record = echo_agent_app_core::tasks::task_runtime::UsageRecord {
                    id: uuid::Uuid::new_v4().to_string(),
                    session_id: trace_session_id.to_string(),
                    run_id: Some(run_id.to_string()),
                    worker_id: Some("main".to_string()),
                    model: model.clone(),
                    provider: provider_name.clone(),
                    route_kind: Some(route.to_string()),
                    input_tokens: *prompt_tokens as u64,
                    output_tokens: *completion_tokens as u64,
                    cached_input_tokens: *cached_prompt_tokens as u64,
                    cache_creation_input_tokens: *cache_creation_prompt_tokens as u64,
                    usage_reported: *usage_reported,
                    system_prompt_hash: Some(sys_prompt_hash.to_string()),
                    tools_schema_hash: Some(tools_hash.to_string()),
                    cwd_hash: cwd_hash.clone(),
                    worker_prompt_hash: None,
                    created_at: chrono::Utc::now(),
                };
                let _ = store.insert_usage_record(&record);
            }
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
        AgentEvent::ContextCompressed {
            before_count,
            after_count,
            before_tokens,
            after_tokens,
        } => Some(ChatEvent::ContextCompressed {
            before_count: *before_count,
            after_count: *after_count,
            before_tokens: *before_tokens,
            after_tokens: *after_tokens,
        }),
        AgentEvent::ToolCall {
            call_id,
            name,
            args,
        } => {
            let _ = emit_chat_event(
                app,
                &ChatEvent::RunStatus {
                    status: "using_tool".to_string(),
                },
                message_key,
                conversation_id,
            );
            Some(ChatEvent::ToolStart {
                call_id: call_id.clone(),
                name: name.clone(),
                args: args.clone(),
            })
        }
        AgentEvent::ToolStream {
            call_id,
            event: ToolStreamEvent::Progress { message, percent },
            ..
        } => Some(ChatEvent::ToolProgress {
            call_id: call_id.clone(),
            message: message.clone(),
            percent: *percent,
        }),
        AgentEvent::ToolStream {
            call_id,
            event: ToolStreamEvent::Output { channel, chunk },
            ..
        } => Some(ChatEvent::ToolOutput {
            call_id: call_id.clone(),
            channel: match channel {
                ToolOutputChannel::Stdout => "stdout",
                ToolOutputChannel::Stderr => "stderr",
                ToolOutputChannel::Log => "log",
            }
            .to_string(),
            chunk: chunk.clone(),
        }),
        AgentEvent::ToolStream {
            call_id,
            event: ToolStreamEvent::Complete(result),
            ..
        } => Some(ChatEvent::ToolComplete {
            call_id: call_id.clone(),
            success: result.success,
            metadata: result.metadata.clone(),
            truncated: result.truncated,
        }),
        AgentEvent::ToolResult {
            call_id,
            name,
            output,
        } => Some(ChatEvent::ToolResult {
            call_id: call_id.clone(),
            name: name.clone(),
            result: output.clone(),
            success: true,
        }),
        AgentEvent::ToolError {
            call_id,
            name,
            error,
        } => Some(ChatEvent::ToolResult {
            call_id: call_id.clone(),
            name: name.clone(),
            result: error.clone(),
            success: false,
        }),
        AgentEvent::ToolBatchStart { tool_count } => Some(ChatEvent::ToolBatchStart {
            tool_count: *tool_count,
        }),
        AgentEvent::ToolBatchEnd => Some(ChatEvent::ToolBatchEnd),
        AgentEvent::Chart { spec } => Some(ChatEvent::Chart { spec: spec.clone() }),
        AgentEvent::FinalAnswer(data) => Some(ChatEvent::FinalAnswer { data: data.clone() }),
        AgentEvent::Cancelled => Some(ChatEvent::Cancelled),
        AgentEvent::Error { source, message } => Some(ChatEvent::Error {
            message: format!("{source}: {message}"),
        }),
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
    let prompt_assembly = state.app_state.trace.prompt_assembly.read().await.clone();
    let context_handle = state
        .app_state
        .connection
        .primary_agent()
        .read(|agent| agent.context().clone())
        .await;
    let context_snapshot = {
        let context = context_handle.lock().await;
        serde_json::json!({
            "message_count": context.messages().len(),
            "estimated_tokens": context.token_estimate(),
            "protected_message_count": context.protected_message_count(),
            "protected_tokens": context.protected_token_estimate(),
        })
    };
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
        "fingerprint_changes": diagnostics.fingerprint_changes,
        "prompt_assembly": prompt_assembly,
        "context_snapshot": context_snapshot,
    }))
}

#[cfg(test)]
mod tool_transport_tests {
    use super::ChatEvent;

    #[test]
    fn tool_output_transport_preserves_call_id_and_channel() -> Result<(), String> {
        for channel in ["stdout", "stderr", "log"] {
            let value = serde_json::to_value(ChatEvent::ToolOutput {
                call_id: "call-42".to_string(),
                channel: channel.to_string(),
                chunk: "你好🙂".to_string(),
            })
            .map_err(|error| error.to_string())?;
            assert_eq!(
                value.get("type").and_then(serde_json::Value::as_str),
                Some("tool_output")
            );
            assert_eq!(
                value.get("call_id").and_then(serde_json::Value::as_str),
                Some("call-42")
            );
            assert_eq!(
                value.get("channel").and_then(serde_json::Value::as_str),
                Some(channel)
            );
        }
        Ok(())
    }
}
