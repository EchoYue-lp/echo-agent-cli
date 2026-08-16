//! Tauri IPC commands for chat streaming.
//!
//! Uses `app.emit()` to stream `AgentEvent` items to the frontend,
//! replacing the WebSocket transport from the Axum server.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use echo_agent::prelude::AgentEvent;
use echo_agent::tools::{ToolFailure, ToolOutputChannel, ToolStreamEvent};
use echo_agent_app_core::chat_driver::ChatDriverEvent;
use echo_agent_app_core::chat_driver::ChatSink;
use echo_agent_app_core::tasks::task_runtime::executor::{ExecEvent, ExecEventScope};
use echo_agent_app_core::tasks::task_runtime::types::RuntimeEventKind;
use echo_agent_app_core::tool_execution::{
    ToolExecutionDetailChannel, ToolExecutionOwner, ToolExecutionRepository, ToolExecutionSummary,
};
use futures::future::BoxFuture;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, MutexGuard as StdMutexGuard};
use tauri::Emitter;
use tokio::sync::oneshot;
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
    /// Auto-compact 后通知前端：Snapshot 置空，Accumulator 保留。
    #[serde(rename = "context_compressed")]
    ContextCompressed {
        before_count: usize,
        after_count: usize,
        before_tokens: usize,
        after_tokens: usize,
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
    #[serde(rename = "notice")]
    Notice {
        level: String,
        code: String,
        message: String,
    },
    #[serde(rename = "execution_path")]
    ExecutionPath {
        requested_mode: String,
        observed_path: String,
    },
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

/// Emit an event on the unified `execution://event` channel. `kind` is `run`,
/// `task`, or `subagent`.
///
/// `subagent_run_id` is the aggregation key the frontend store uses to group a
/// Subagent's events into one card. It is the concrete execution id
/// (for formal PlanTasks, `{run_id}:{task_id}:{plan_revision}:{attempt}`), never the
/// stable PlanTask id. For non-Subagent events pass `""`; this function only
/// attaches the field for `kind == "subagent"`.
pub(crate) fn emit_execution_event(
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

pub(crate) fn emit_tool_execution_summary(
    app: &tauri::AppHandle,
    event: &str,
    agent: &str,
    summary: &ToolExecutionSummary,
) -> bool {
    let payload = match serde_json::to_value(summary) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(%error, "failed to serialize tool execution summary");
            return false;
        }
    };
    emit_execution_event(
        app,
        summary.run_id.as_deref().unwrap_or(""),
        "tool",
        event,
        agent,
        "",
        payload,
    );
    true
}

/// Global pending map for approval/input responses.
#[allow(clippy::type_complexity)]
static PENDING_RESPONSES: LazyLock<Arc<StdMutex<HashMap<String, PendingRequest>>>> =
    LazyLock::new(|| Arc::new(StdMutex::new(HashMap::new())));

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
    Cancelled {
        reason: String,
    },
}

/// Removes a pending GUI request synchronously if its provider future is
/// dropped before a response or timeout consumes the entry.
struct PendingResponseReservation {
    pending: Arc<StdMutex<HashMap<String, PendingRequest>>>,
    request_id: String,
}

impl PendingResponseReservation {
    fn insert(
        pending: Arc<StdMutex<HashMap<String, PendingRequest>>>,
        request_id: String,
        request: PendingRequest,
    ) -> Self {
        lock_std(&pending, "pending GUI HITL responses").insert(request_id.clone(), request);
        Self {
            pending,
            request_id,
        }
    }
}

impl Drop for PendingResponseReservation {
    fn drop(&mut self) {
        let request =
            lock_std(&self.pending, "pending GUI HITL responses").remove(&self.request_id);
        if let Some(request) = request {
            let _ = request.tx.send(PendingResponse::Cancelled {
                reason: "HITL request owner was cancelled".to_string(),
            });
        }
    }
}

pub(crate) async fn cancel_pending_hitl(message_key: Option<&str>, reason: &str) -> usize {
    let mut pending = lock_std(&PENDING_RESPONSES, "pending GUI HITL responses");
    let request_ids = pending
        .iter()
        .filter(|(_, request)| message_key.is_none_or(|key| request.message_key == key))
        .map(|(request_id, _)| request_id.clone())
        .collect::<Vec<_>>();
    let mut cancelled = 0usize;
    for request_id in request_ids {
        let Some(request) = pending.remove(&request_id) else {
            continue;
        };
        let _ = request.tx.send(PendingResponse::Cancelled {
            reason: reason.to_string(),
        });
        cancelled = cancelled.saturating_add(1);
        tracing::debug!(%request_id, "cancelled pending HITL request");
    }
    cancelled
}

/// Tauri-based HumanLoopProvider — emits approval/input requests via Tauri events
/// and awaits responses through the shared PENDING_RESPONSES map.
pub(crate) struct TauriHumanLoopHandler {
    app_handle: tauri::AppHandle,
    pending: Arc<StdMutex<HashMap<String, PendingRequest>>>,
    conversation_id: Option<String>,
    message_key: String,
}

impl TauriHumanLoopHandler {
    pub(crate) fn new(
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
                    let _reservation = PendingResponseReservation::insert(
                        pending.clone(),
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
                                            Some("session_tool") => {
                                                Ok(HumanLoopResponse::ApprovedWithScope {
                                                    scope: echo_agent::human_loop::ApprovalScope::SessionTool,
                                                })
                                            }
                                            _ => Ok(HumanLoopResponse::Approved),
                                        }
                                    } else {
                                        Ok(HumanLoopResponse::Rejected { reason })
                                    }
                                }
                                Ok(PendingResponse::Cancelled { reason }) => {
                                    Ok(HumanLoopResponse::Rejected { reason: Some(reason) })
                                }
                                _ => Ok(HumanLoopResponse::Timeout),
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {
                            lock_std(&pending, "pending GUI HITL responses").remove(&request_id);
                            Ok(HumanLoopResponse::Timeout)
                        }
                    }
                }
                echo_agent::human_loop::HumanLoopKind::Input => {
                    let event = ChatEvent::InputRequest {
                        request_id: request_id.clone(),
                        prompt: req.prompt.clone(),
                    };
                    let _reservation = PendingResponseReservation::insert(
                        pending.clone(),
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
                                Ok(PendingResponse::Cancelled { reason }) => {
                                    Ok(HumanLoopResponse::Rejected { reason: Some(reason) })
                                }
                                _ => Ok(HumanLoopResponse::Text(String::new())),
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {
                            lock_std(&pending, "pending GUI HITL responses").remove(&request_id);
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
                    let _reservation = PendingResponseReservation::insert(
                        pending.clone(),
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
                                Ok(PendingResponse::Cancelled { reason }) => {
                                    Ok(HumanLoopResponse::Rejected { reason: Some(reason) })
                                }
                                _ => Ok(HumanLoopResponse::Timeout),
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {
                            lock_std(&pending, "pending GUI HITL responses").remove(&request_id);
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
    // LLM sees images/files via the unified PreparedUserTurn (instruction + input
    // resources). Attachments are persisted first, then converted to refs; the
    // turn's to_message() rebuilds the multimodal Message from disk (the refs
    // path), so the in-memory `build_message` helper is no longer used here.
    let saved_attachments = attachments.unwrap_or_default();
    let ws_root = state.app_state.current_workspace().await.map(|ws| ws.root);
    let attachment_refs: Vec<echo_agent_app_core::attachments::AttachmentRef> = if saved_attachments
        .is_empty()
    {
        Vec::new()
    } else {
        let uploads_dir = echo_agent_app_core::attachments::resolve_uploads_dir(ws_root.as_deref());
        let saved =
            echo_agent_app_core::attachments::save_attachments(&saved_attachments, &uploads_dir);
        // Build refs (path + name + mime) for binding to the run so plan-level
        // subagents can rebuild the multimodal message later, and so the
        // PreparedUserTurn can re-read them for inline delivery.
        saved
            .iter()
            .map(|(path, att)| {
                echo_agent_app_core::attachments::AttachmentRef::from_saved(path.clone(), att)
            })
            .collect()
    };
    if !attachment_refs.is_empty() {
        tracing::info!(
            count = attachment_refs.len(),
            "send_chat_message: multimodal message with attachments"
        );
    }

    let message_key = message_key.unwrap_or_else(|| Uuid::new_v4().to_string());

    // ── Interrupt detection ─────────────────────────────────────────────
    // If the same conversation already has an in-progress (Running/Paused)
    // run, do NOT start a new one. Instead, emit an InterruptPrompt event
    // so the GUI can ask the user what to do (resume / edit-and-resume /
    // abandon).
    if let Some(ref conv_id) = conversation_id
        && let Some(store) = state.app_state.tasks.runtime.as_ref()
        && let Ok(Some(existing)) = store.find_in_progress_run_by_conversation(conv_id)
        && existing.status == echo_agent_app_core::tasks::task_runtime::TaskRunStatus::Running
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
    let foreground_lease = state
        .app_state
        .session
        .foreground_turns
        .begin(
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Gui,
            active_turn_key.clone(),
            message_key.clone(),
        )
        .map_err(|error| match error {
            echo_agent_app_core::foreground_turn::ForegroundTurnError::Busy {
                active_turn_id,
                ..
            } => IpcError::Validation(format!("chat_turn_busy:{active_turn_id}")),
            other => IpcError::Validation(other.to_string()),
        })?;
    let cancel_token = foreground_lease.cancellation_token();

    // Foreground admission must precede pool admission. Retain the pool
    // receipt in the spawned turn until the shared driver and HITL reset have
    // both settled, so workspace publication cannot race an issued handle.
    let pool_execution = match conversation_id.as_deref() {
        Some(conversation_id) => Some(
            state
                .app_state
                .connection
                .agent_for(conversation_id)
                .await
                .map_err(|error| IpcError::Validation(error.to_string()))?,
        ),
        None => None,
    };
    let agent_handle = pool_execution
        .as_ref()
        .map(echo_agent_app_core::agent_pool::AgentPoolExecutionLease::agent)
        .unwrap_or_else(|| state.app_state.connection.primary_agent());

    // Ensure stable cache_user_id for KVCache isolation (DeepSeek requires this
    // for prompt cache reuse across requests; without it, cache hit rate drops
    // to <1% because every request is treated as from a different user).
    // Persisted to ~/.eko/cache_user_id — generated once, reused forever.
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
    let sink = tauri_chat_sink(
        app.clone(),
        message_key.clone(),
        conversation_id.clone(),
        state.app_state.storage.tool_executions.clone(),
        state.app_state.tasks.runtime.clone(),
    );
    // Signal the chat-turn lifecycle so the GUI shows the spinner / terminal
    // badge. Ordinary chat turns are not TaskRuntime runs.
    let _ = sink.on_event(ChatDriverEvent::TurnStatus {
        status: "running".to_string(),
    });

    let agent_handle_clone = agent_handle.clone();
    // Build the prepared turn (instruction + input resources, mode hint folded
    // in, long pastes spilled to user-input artifacts). Replaces the old
    // (message, multimodal_message) pair handed to drive_chat.
    let mode_hint_for_turn = interaction_mode.prompt_hint().to_string();
    let spill_dir =
        echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(ws_root.as_deref());
    let prepared_turn = match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::prepared_turn::UserTurnInput {
            text: &message,
            attachments: &attachment_refs,
            mode_hint: Some(&mode_hint_for_turn),
            spill_dir: &spill_dir,
            conversation_id: conversation_id.as_deref(),
            turn_id: Some(&message_key),
        },
    ) {
        Ok(turn) => turn,
        Err(e) => {
            tracing::warn!(error = %e, "failed to prepare user turn");
            foreground_lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message("prepared_turn", e.to_string()),
            ));
            let _ = sink.on_event(ChatDriverEvent::TurnStatus {
                status: "failed".to_string(),
            });
            return Err(IpcError::Validation(format!(
                "failed to prepare user turn: {e}"
            )));
        }
    };
    let res = std::sync::Arc::new(echo_agent_app_core::chat_resources::ChatResources {
        pool: state.app_state.connection.pool.clone(),
        store: state.app_state.tasks.runtime.clone(),
        sink: sink.clone(),
        webhook_emitter: Some(state.app_state.webhook.emitter.clone()),
        conv_id: Some(active_turn_key.clone()),
        root_message_id: message_key.clone(),
        attachments: prepared_turn.inline_attachment_refs(),
        cancel: cancel_token.clone(),
        mode_hint,
        interaction_mode,
        review_integration: state.app_state.review_integration.clone(),
        layer_manager: None,
        memory_generation: None,
        human_loop_provider: Some(hitl_handler),
    });
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        // The prepared turn carries instruction + inline resources (images /
        // files re-read from disk via refs). Background runs created by
        // create_complex_task pick up attachments via ChatResources.attachments
        // (already bound above).
        let outcome = echo_agent_app_core::foreground_turn::drive_foreground_chat(
            foreground_lease,
            &agent_handle_clone,
            &prepared_turn,
            res,
        )
        .await;
        let terminal_status = match &outcome {
            Ok(outcome) => outcome.status(),
            Err(_) => "failed",
        };
        if let Err(e) = &outcome {
            tracing::warn!(error = %e, "drive_chat chat turn errored");
        }
        // Release all execution ownership before emitting Done. The frontend
        // may immediately dispatch the next queued turn when it receives it.
        let _ = sink.on_event(ChatDriverEvent::TurnStatus {
            status: terminal_status.to_string(),
        });
        agent_handle_clone
            .write_async(|agent| {
                Box::pin(async move {
                    let empty = Arc::new(echo_agent_app_core::hitl::HitlDispatcher::new());
                    agent.set_human_loop_provider_preserving_approvals(empty);
                })
            })
            .await;
        drop(pool_execution);
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
        .foreground_turns
        .snapshot(
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Gui,
            &conversation_id,
        )
        .map(|snapshot| snapshot.turn_id)
        .ok_or_else(|| IpcError::Validation("no active chat turn".to_string()))?;
    let saved_attachments = attachments.unwrap_or_default();
    let ws_root = state.app_state.current_workspace().await.map(|ws| ws.root);
    let uploads_dir = echo_agent_app_core::attachments::resolve_uploads_dir(ws_root.as_deref());
    let saved =
        echo_agent_app_core::attachments::save_attachments(&saved_attachments, &uploads_dir);
    let attachment_refs: Vec<_> = saved
        .iter()
        .map(|(path, att)| {
            echo_agent_app_core::attachments::AttachmentRef::from_saved(path.clone(), att)
        })
        .collect();
    let spill_dir =
        echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(ws_root.as_deref());
    let prepared = echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::prepared_turn::UserTurnInput {
            text: &message,
            attachments: &attachment_refs,
            mode_hint: None,
            spill_dir: &spill_dir,
            conversation_id: Some(&conversation_id),
            turn_id: Some(&expected_turn_id),
        },
    )
    .map_err(|error| IpcError::Validation(error.to_string()))?;
    let steer_message = prepared
        .to_message()
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let agent_execution = state
        .app_state
        .connection
        .agent_for(&conversation_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let agent = agent_execution.agent();
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

fn select_active_chat_turn(
    control: &echo_agent_app_core::foreground_turn::ForegroundTurnControl,
    conversation_id: Option<&str>,
) -> Result<Option<echo_agent_app_core::foreground_turn::ForegroundTurnSnapshot>, IpcError> {
    use echo_agent_app_core::foreground_turn::ForegroundTurnSurface;

    if let Some(conversation_id) = conversation_id {
        return Ok(control.snapshot(ForegroundTurnSurface::Gui, conversation_id));
    }
    let mut snapshots = control
        .snapshots(ForegroundTurnSurface::Gui)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    match snapshots.len() {
        0 => Ok(None),
        1 => Ok(snapshots.pop()),
        _ => Err(IpcError::Validation(
            "active_chat_turn_ambiguous:conversation_id_required".to_string(),
        )),
    }
}

/// Restore the exact registry scope after a WebView/hook remount.
///
/// If no product conversation id exists yet, fallback is permitted only when
/// there is exactly one active GUI turn. The returned `conversation_id` is the
/// registry's real scope key and may therefore be `message:<turn_id>`.
#[tauri::command]
pub fn get_active_chat_turn(
    state: tauri::State<'_, TauriState>,
    conversation_id: Option<String>,
) -> Result<Option<echo_agent_app_core::foreground_turn::ForegroundTurnSnapshot>, IpcError> {
    select_active_chat_turn(
        &state.app_state.session.foreground_turns,
        conversation_id.as_deref(),
    )
}

/// Cancel an active chat stream.
#[tauri::command]
pub async fn cancel_chat(
    state: tauri::State<'_, TauriState>,
    conversation_id: String,
    message_key: String,
) -> Result<serde_json::Value, IpcError> {
    let waiter = state
        .app_state
        .session
        .foreground_turns
        .request_cancel(
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Gui,
            &conversation_id,
            &message_key,
        )
        .map_err(|error| IpcError::Validation(error.to_string()))?;

    // Reject pending HITL before waiting so parked execution can reach its
    // terminal outcome. Ownership remains registered until that settlement.
    cancel_pending_hitl(Some(&message_key), "cancelled by user").await;
    let settlement = waiter
        .wait()
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;

    Ok(serde_json::json!({
        "success": true,
        "turn_id": settlement.turn_id,
        "status": settlement.outcome.status(),
    }))
}

/// Respond to an approval request.
#[tauri::command]
pub async fn send_approval_response(
    request_id: String,
    approved: bool,
    reason: Option<String>,
    scope: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let req = lock_std(&PENDING_RESPONSES, "pending GUI HITL responses").remove(&request_id);
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
    let req = lock_std(&PENDING_RESPONSES, "pending GUI HITL responses").remove(&request_id);
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
    let req = lock_std(&PENDING_RESPONSES, "pending GUI HITL responses").remove(&request_id);
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

/// Projects app-owned TaskRuntime tool events into the same durable repository
/// and `kind=tool` channel used by ordinary chat and framework Subagents.
pub(crate) struct TauriExecutionProjector {
    app: Option<tauri::AppHandle>,
    tool_executions: Arc<ToolExecutionRepository>,
    task_runtime_store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    pending_tool_completions: StdMutex<HashMap<String, PendingToolCompletion>>,
    active_tool_ids_by_execution: StdMutex<HashMap<String, HashSet<String>>>,
}

impl TauriExecutionProjector {
    pub(crate) fn new(
        app: tauri::AppHandle,
        tool_executions: Arc<ToolExecutionRepository>,
        task_runtime_store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    ) -> Self {
        Self {
            app: Some(app),
            tool_executions,
            task_runtime_store,
            pending_tool_completions: StdMutex::new(HashMap::new()),
            active_tool_ids_by_execution: StdMutex::new(HashMap::new()),
        }
    }

    pub(crate) fn emit(&self, event: ExecEvent) {
        self.project_tool_event(&event);
        if let Some(app) = self.app.as_ref() {
            emit_tauri_execution_event(app, event);
        }
    }

    #[cfg(test)]
    fn without_app(
        tool_executions: Arc<ToolExecutionRepository>,
        task_runtime_store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    ) -> Self {
        Self {
            app: None,
            tool_executions,
            task_runtime_store,
            pending_tool_completions: StdMutex::new(HashMap::new()),
            active_tool_ids_by_execution: StdMutex::new(HashMap::new()),
        }
    }

    fn emit_summary(&self, event: &str, agent: &str, summary: &ToolExecutionSummary) {
        if let Some(app) = self.app.as_ref() {
            let _ = emit_tool_execution_summary(app, event, agent, summary);
        }
    }

    fn conversation_id(&self, run_id: &str) -> Option<String> {
        self.task_runtime_store
            .as_ref()
            .and_then(|store| store.get_run(run_id).ok())
            .flatten()
            .map(|run| run.conversation_id)
    }

    fn completion_key(subagent_run_id: &str, call_id: &str) -> String {
        format!("{subagent_run_id}\0{call_id}")
    }

    fn project_tool_event(&self, event: &ExecEvent) {
        if event.scope != ExecEventScope::Subagent {
            return;
        }
        let Some(subagent_run_id) = event.subagent_run_id.as_deref() else {
            return;
        };
        let owner = ToolExecutionOwner::Subagent {
            subagent_run_id: subagent_run_id.to_string(),
        };
        let payload = match event.payload.as_object() {
            Some(payload) => payload,
            None => return,
        };
        let agent = event.agent.as_deref().unwrap_or("echo-assistant");

        match event.event {
            RuntimeEventKind::ToolStarted => {
                let Some(call_id) = payload.get("call_id").and_then(serde_json::Value::as_str)
                else {
                    return;
                };
                let Some(name) = payload.get("name").and_then(serde_json::Value::as_str) else {
                    return;
                };
                let args = payload
                    .get("args")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let conversation_id = self.conversation_id(&event.run_id);
                match self.tool_executions.start(
                    owner,
                    conversation_id.as_deref(),
                    Some(&event.run_id),
                    call_id,
                    name,
                    &args,
                ) {
                    Ok(summary) => {
                        lock_std(
                            &self.active_tool_ids_by_execution,
                            "active TaskRuntime Subagent tools",
                        )
                        .entry(subagent_run_id.to_string())
                        .or_default()
                        .insert(call_id.to_string());
                        self.emit_summary("started", agent, &summary);
                    }
                    Err(error) => {
                        tracing::warn!(%error, %call_id, %name, "failed to persist TaskRuntime Subagent tool start");
                    }
                }
            }
            RuntimeEventKind::ToolOutput => {
                let Some(call_id) = payload.get("call_id").and_then(serde_json::Value::as_str)
                else {
                    return;
                };
                let output = payload
                    .get("chunk")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| payload.get("message").and_then(serde_json::Value::as_str));
                let Some(output) = output else {
                    return;
                };
                let channel = match payload.get("channel").and_then(serde_json::Value::as_str) {
                    Some("stdout") => ToolExecutionDetailChannel::Stdout,
                    Some("stderr") => ToolExecutionDetailChannel::Stderr,
                    _ => ToolExecutionDetailChannel::Log,
                };
                if let Err(error) = self
                    .tool_executions
                    .append_output(&owner, call_id, channel, output)
                {
                    tracing::warn!(%error, %call_id, "failed to persist TaskRuntime Subagent tool output");
                }
            }
            RuntimeEventKind::ToolCompleted => {
                let Some(call_id) = payload.get("call_id").and_then(serde_json::Value::as_str)
                else {
                    return;
                };
                let completion_key = Self::completion_key(subagent_run_id, call_id);
                let metadata = payload
                    .get("metadata")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<HashMap<String, String>>(value).ok())
                    .unwrap_or_default();
                let truncated = payload
                    .get("truncated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let Some(result) = payload.get("result").and_then(serde_json::Value::as_str) else {
                    lock_std(
                        &self.pending_tool_completions,
                        "pending TaskRuntime tool completions",
                    )
                    .insert(
                        completion_key,
                        PendingToolCompletion {
                            metadata,
                            truncated,
                        },
                    );
                    return;
                };
                let pending = lock_std(
                    &self.pending_tool_completions,
                    "pending TaskRuntime tool completions",
                )
                .remove(&completion_key)
                .unwrap_or_default();
                let mut combined_metadata = pending.metadata;
                combined_metadata.extend(metadata);
                let success = payload
                    .get("success")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                let failure = payload
                    .get("failure")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ToolFailure>(value).ok());
                match self.tool_executions.finish(
                    &owner,
                    call_id,
                    success,
                    result,
                    failure,
                    combined_metadata,
                    truncated || pending.truncated,
                ) {
                    Ok(summary) => {
                        let mut active_tools = lock_std(
                            &self.active_tool_ids_by_execution,
                            "active TaskRuntime Subagent tools",
                        );
                        if let Some(call_ids) = active_tools.get_mut(subagent_run_id) {
                            call_ids.remove(call_id);
                            if call_ids.is_empty() {
                                active_tools.remove(subagent_run_id);
                            }
                        }
                        self.emit_summary("finished", agent, &summary);
                    }
                    Err(error) => {
                        tracing::warn!(%error, %call_id, "failed to persist TaskRuntime Subagent tool completion");
                    }
                }
            }
            RuntimeEventKind::Completed
            | RuntimeEventKind::Failed
            | RuntimeEventKind::Cancelled
            | RuntimeEventKind::TimedOut => {
                self.cancel_active_tools(subagent_run_id, agent, &owner);
            }
            _ => {}
        }
    }

    fn cancel_active_tools(&self, subagent_run_id: &str, agent: &str, owner: &ToolExecutionOwner) {
        let call_ids = lock_std(
            &self.active_tool_ids_by_execution,
            "active TaskRuntime Subagent tools",
        )
        .remove(subagent_run_id)
        .unwrap_or_default();
        for call_id in call_ids {
            lock_std(
                &self.pending_tool_completions,
                "pending TaskRuntime tool completions",
            )
            .remove(&Self::completion_key(subagent_run_id, &call_id));
            match self.tool_executions.cancel(owner, &call_id) {
                Ok(summary) => {
                    self.emit_summary("cancelled", agent, &summary);
                }
                Err(error) => {
                    tracing::warn!(%error, %call_id, "failed to cancel TaskRuntime Subagent tool");
                }
            }
        }
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
    tool_executions: Arc<echo_agent_app_core::tool_execution::ToolExecutionRepository>,
    tool_completions: StdMutex<HashMap<String, PendingToolCompletion>>,
    active_tool_ids: StdMutex<HashSet<String>>,
    execution_projector: Arc<TauriExecutionProjector>,
}

pub(crate) fn tauri_chat_sink(
    app: tauri::AppHandle,
    message_key: String,
    conversation_id: Option<String>,
    tool_executions: Arc<echo_agent_app_core::tool_execution::ToolExecutionRepository>,
    runtime_store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
) -> Arc<dyn ChatSink> {
    let execution_projector = Arc::new(TauriExecutionProjector::new(
        app.clone(),
        tool_executions.clone(),
        runtime_store,
    ));
    Arc::new(TauriChatSink {
        app,
        message_key,
        conversation_id,
        tool_executions,
        tool_completions: StdMutex::new(HashMap::new()),
        active_tool_ids: StdMutex::new(HashSet::new()),
        execution_projector,
    })
}

#[derive(Default)]
struct PendingToolCompletion {
    metadata: HashMap<String, String>,
    truncated: bool,
}

impl TauriChatSink {
    fn tool_owner(&self) -> ToolExecutionOwner {
        ToolExecutionOwner::Chat {
            message_id: self.message_key.clone(),
        }
    }

    fn cancel_active_tools(&self, owner: &ToolExecutionOwner) {
        let call_ids = lock_std(&self.active_tool_ids, "active GUI tools")
            .drain()
            .collect::<Vec<_>>();
        for call_id in call_ids {
            lock_std(&self.tool_completions, "GUI tool completions").remove(&call_id);
            match self.tool_executions.cancel(owner, &call_id) {
                Ok(summary) => {
                    let _ = emit_tool_execution_summary(
                        &self.app,
                        "cancelled",
                        "echo-assistant",
                        &summary,
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, %call_id, "failed to cancel persisted tool");
                }
            }
        }
    }

    fn handle_tool_event(&self, event: &AgentEvent) -> Option<bool> {
        let owner = self.tool_owner();
        match event {
            AgentEvent::ToolCall {
                call_id,
                name,
                args,
            } => {
                let _ = emit_chat_event(
                    &self.app,
                    &ChatEvent::RunStatus {
                        status: "using_tool".to_string(),
                    },
                    &self.message_key,
                    &self.conversation_id,
                );
                let summary = match self.tool_executions.start(
                    owner,
                    self.conversation_id.as_deref(),
                    Some(&self.message_key),
                    call_id,
                    name,
                    args,
                ) {
                    Ok(summary) => summary,
                    Err(error) => {
                        tracing::warn!(%error, %call_id, %name, "failed to persist tool start");
                        return Some(true);
                    }
                };
                lock_std(&self.active_tool_ids, "active GUI tools").insert(call_id.clone());
                Some(emit_tool_execution_summary(
                    &self.app,
                    "started",
                    "echo-assistant",
                    &summary,
                ))
            }
            AgentEvent::ToolStream {
                call_id,
                event: ToolStreamEvent::Output { channel, chunk },
                ..
            } => {
                let detail_channel = match channel {
                    ToolOutputChannel::Stdout => ToolExecutionDetailChannel::Stdout,
                    ToolOutputChannel::Stderr => ToolExecutionDetailChannel::Stderr,
                    ToolOutputChannel::Log => ToolExecutionDetailChannel::Log,
                };
                if let Err(error) =
                    self.tool_executions
                        .append_output(&owner, call_id, detail_channel, chunk)
                {
                    tracing::warn!(%error, %call_id, "failed to persist tool output chunk");
                }
                Some(true)
            }
            AgentEvent::ToolStream {
                call_id,
                event: ToolStreamEvent::Complete(result),
                ..
            } => {
                lock_std(&self.tool_completions, "GUI tool completions").insert(
                    call_id.clone(),
                    PendingToolCompletion {
                        metadata: result.metadata.clone(),
                        truncated: result.truncated,
                    },
                );
                Some(true)
            }
            AgentEvent::ToolStream { .. } => Some(true),
            AgentEvent::ToolResult {
                call_id, output, ..
            } => {
                let completion = lock_std(&self.tool_completions, "GUI tool completions")
                    .remove(call_id)
                    .unwrap_or_default();
                let summary = match self.tool_executions.finish(
                    &owner,
                    call_id,
                    true,
                    output,
                    None,
                    completion.metadata,
                    completion.truncated,
                ) {
                    Ok(summary) => summary,
                    Err(error) => {
                        tracing::warn!(%error, %call_id, "failed to persist tool completion");
                        return Some(true);
                    }
                };
                lock_std(&self.active_tool_ids, "active GUI tools").remove(call_id);
                Some(emit_tool_execution_summary(
                    &self.app,
                    "finished",
                    "echo-assistant",
                    &summary,
                ))
            }
            AgentEvent::ToolError {
                call_id,
                error,
                failure,
                ..
            } => {
                let completion = lock_std(&self.tool_completions, "GUI tool completions")
                    .remove(call_id)
                    .unwrap_or_default();
                let summary = match self.tool_executions.finish(
                    &owner,
                    call_id,
                    false,
                    error,
                    Some(failure.clone()),
                    completion.metadata,
                    completion.truncated,
                ) {
                    Ok(summary) => summary,
                    Err(persist_error) => {
                        tracing::warn!(%persist_error, %call_id, "failed to persist tool failure");
                        return Some(true);
                    }
                };
                lock_std(&self.active_tool_ids, "active GUI tools").remove(call_id);
                Some(emit_tool_execution_summary(
                    &self.app,
                    "finished",
                    "echo-assistant",
                    &summary,
                ))
            }
            AgentEvent::Cancelled | AgentEvent::Error { .. } => {
                self.cancel_active_tools(&owner);
                None
            }
            _ => None,
        }
    }
}

fn lock_std<'a, T>(mutex: &'a StdMutex<T>, label: &str) -> StdMutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::warn!(%label, "GUI tool projection lock was poisoned; recovering state");
        poisoned.into_inner()
    })
}

impl echo_agent_app_core::chat_driver::ChatSink for TauriChatSink {
    fn on_event(&self, event: ChatDriverEvent) -> bool {
        match event {
            ChatDriverEvent::Agent(event) => {
                if let Some(emitted) = self.handle_tool_event(&event.payload) {
                    return emitted;
                }
                let chat_event = agent_event_to_chat_event(
                    &self.app,
                    &event.payload,
                    &self.message_key,
                    &self.conversation_id,
                );
                emit_chat_event(
                    &self.app,
                    &chat_event,
                    &self.message_key,
                    &self.conversation_id,
                )
            }
            ChatDriverEvent::Execution(event) => {
                self.execution_projector.emit(event);
                true
            }
            ChatDriverEvent::TurnStatus { status } => {
                if status != "running" {
                    self.cancel_active_tools(&self.tool_owner());
                }
                let emitted = emit_chat_event(
                    &self.app,
                    &ChatEvent::RunStatus {
                        status: status.clone(),
                    },
                    &self.message_key,
                    &self.conversation_id,
                );
                if status == "running" {
                    emitted
                } else {
                    emitted
                        && emit_chat_event(
                            &self.app,
                            &ChatEvent::Done,
                            &self.message_key,
                            &self.conversation_id,
                        )
                }
            }
            ChatDriverEvent::ExecutionPath {
                requested_mode,
                observed_path,
            } => emit_chat_event(
                &self.app,
                &ChatEvent::ExecutionPath {
                    requested_mode,
                    observed_path,
                },
                &self.message_key,
                &self.conversation_id,
            ),
            ChatDriverEvent::Interrupt {
                run_id,
                goal,
                new_message,
            } => emit_chat_event(
                &self.app,
                &ChatEvent::InterruptPrompt {
                    run_id,
                    goal,
                    new_message,
                },
                &self.message_key,
                &self.conversation_id,
            ),
        }
    }
}

fn emit_tauri_execution_event(app: &tauri::AppHandle, event: ExecEvent) {
    let ExecEvent {
        run_id,
        scope,
        task_id,
        subagent_run_id,
        event,
        agent,
        mut payload,
    } = event;
    let kind = match scope {
        ExecEventScope::Run => "run",
        ExecEventScope::Task => "task",
        ExecEventScope::Subagent => "subagent",
    };
    if let (Some(task_id), serde_json::Value::Object(fields)) = (&task_id, &mut payload) {
        fields.insert("task_id".into(), task_id.clone().into());
    }
    emit_execution_event(
        app,
        &run_id,
        kind,
        event.as_str(),
        agent.as_deref().unwrap_or("echo-assistant"),
        subagent_run_id.as_deref().unwrap_or(""),
        payload,
    );
}

/// Map an AgentEvent to a ChatEvent.
fn agent_event_to_chat_event(
    app: &tauri::AppHandle,
    event: &AgentEvent,
    message_key: &str,
    conversation_id: &Option<String>,
) -> ChatEvent {
    match event {
        AgentEvent::Token(data) => ChatEvent::Token { data: data.clone() },
        AgentEvent::ThinkStart => {
            let _ = emit_chat_event(
                app,
                &ChatEvent::RunStatus {
                    status: "thinking".to_string(),
                },
                message_key,
                conversation_id,
            );
            ChatEvent::ThinkingStart
        }
        AgentEvent::ThinkEnd {
            prompt_tokens,
            completion_tokens,
        } => ChatEvent::ThinkingEnd {
            prompt_tokens: *prompt_tokens,
            completion_tokens: *completion_tokens,
        },
        AgentEvent::LlmUsage {
            model,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_prompt_tokens,
            cache_creation_prompt_tokens,
            usage_reported,
        } => ChatEvent::LlmUsage {
            model: model.clone(),
            prompt_tokens: *prompt_tokens,
            completion_tokens: *completion_tokens,
            total_tokens: *total_tokens,
            cached_prompt_tokens: *cached_prompt_tokens,
            cache_creation_prompt_tokens: *cache_creation_prompt_tokens,
            usage_reported: *usage_reported,
        },
        AgentEvent::ContextCompressed {
            before_count,
            after_count,
            before_tokens,
            after_tokens,
        } => ChatEvent::ContextCompressed {
            before_count: *before_count,
            after_count: *after_count,
            before_tokens: *before_tokens,
            after_tokens: *after_tokens,
        },
        AgentEvent::ToolCall { .. }
        | AgentEvent::ToolStream { .. }
        | AgentEvent::ToolResult { .. }
        | AgentEvent::ToolError { .. } => ChatEvent::Notice {
            level: "warning".to_string(),
            code: "tool_event_projection_bypassed".to_string(),
            message: "Tool event bypassed the durable execution projection".to_string(),
        },
        AgentEvent::ToolBatchStart { tool_count } => ChatEvent::ToolBatchStart {
            tool_count: *tool_count,
        },
        AgentEvent::ToolBatchEnd => ChatEvent::ToolBatchEnd,
        AgentEvent::BudgetDecision {
            decision,
            reason,
            iteration,
            ..
        } => ChatEvent::Notice {
            level: if matches!(decision, echo_core::agent::BudgetDecision::HardStop) {
                "error"
            } else {
                "info"
            }
            .to_string(),
            code: "budget_decision".to_string(),
            message: format!("{decision:?} at iteration {iteration}: {reason}"),
        },
        AgentEvent::GuardTriggered { guard, blocked } => ChatEvent::Notice {
            level: if *blocked { "warning" } else { "info" }.to_string(),
            code: "guard_triggered".to_string(),
            message: format!("Guard {guard} triggered (blocked={blocked})"),
        },
        AgentEvent::MemoryRecalled { count } => ChatEvent::Notice {
            level: "info".to_string(),
            code: "memory_recalled".to_string(),
            message: format!("Recalled {count} memory item(s)"),
        },
        AgentEvent::Chart { spec } => ChatEvent::Chart { spec: spec.clone() },
        AgentEvent::SafetyNotice {
            action,
            reason,
            risk,
            permission,
        } => ChatEvent::Notice {
            level: "warning".to_string(),
            code: "safety_notice".to_string(),
            message: format!("{action}: {reason} (risk={risk}, permission={permission})"),
        },
        AgentEvent::ParameterError {
            tool,
            parameter,
            expected,
            got,
        } => ChatEvent::Notice {
            level: "error".to_string(),
            code: "parameter_error".to_string(),
            message: format!("{tool}.{parameter}: expected {expected}, got {got}"),
        },
        AgentEvent::FinalAnswer(data) => ChatEvent::FinalAnswer { data: data.clone() },
        AgentEvent::Cancelled => ChatEvent::Cancelled,
        AgentEvent::Error {
            source, message, ..
        } => ChatEvent::Error {
            message: format!("{source}: {message}"),
        },
        other => ChatEvent::Notice {
            level: "info".to_string(),
            code: "unknown_agent_event".to_string(),
            message: format!("{other:?}"),
        },
    }
}

#[cfg(test)]
mod hitl_pending_tests {
    use super::*;

    #[test]
    fn dropping_reservation_removes_pending_request_and_cancels_sender()
    -> Result<(), Box<dyn std::error::Error>> {
        let pending = Arc::new(StdMutex::new(HashMap::new()));
        let (sender, mut receiver) = oneshot::channel();
        let reservation = PendingResponseReservation::insert(
            pending.clone(),
            "request-1".to_string(),
            PendingRequest {
                message_key: "root-message".to_string(),
                tx: sender,
            },
        );
        assert_eq!(
            lock_std(&pending, "test pending GUI HITL responses").len(),
            1
        );

        drop(reservation);

        assert!(lock_std(&pending, "test pending GUI HITL responses").is_empty());
        assert!(matches!(
            receiver.try_recv()?,
            PendingResponse::Cancelled { reason }
                if reason == "HITL request owner was cancelled"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn explicit_cancellation_rejects_matching_pending_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let request_id = Uuid::new_v4().to_string();
        let message_key = format!("root-{}", Uuid::new_v4());
        let (sender, receiver) = oneshot::channel();
        lock_std(&PENDING_RESPONSES, "test pending GUI HITL responses").insert(
            request_id,
            PendingRequest {
                message_key: message_key.clone(),
                tx: sender,
            },
        );

        assert_eq!(
            cancel_pending_hitl(Some(&message_key), "task run paused").await,
            1
        );
        assert!(matches!(
            receiver.await?,
            PendingResponse::Cancelled { reason } if reason == "task run paused"
        ));
        Ok(())
    }
}

#[cfg(test)]
mod execution_projector_tests {
    use super::*;
    use echo_agent_app_core::tasks::task_runtime::{AttendedMode, DomainProfile, TaskRuntimeStore};
    use echo_agent_app_core::tool_execution::ToolExecutionStatus;
    use std::path::{Path, PathBuf};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> std::io::Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "eko-task-runtime-tool-projector-{}",
                Uuid::new_v4()
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                eprintln!("failed to clean projector test directory: {error}");
            }
        }
    }

    #[test]
    fn task_runtime_tools_are_persisted_with_output_and_terminal_cleanup()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TestDir::new()?;
        let repository = Arc::new(ToolExecutionRepository::open(temp.path())?);
        let runtime = Arc::new(TaskRuntimeStore::new_in_memory()?);
        runtime.create_run(
            "run-1",
            "workspace-1",
            "conversation-1",
            "message-1",
            DomainProfile::AiCoding,
            "analyze project",
            "formal_plan",
            AttendedMode::Attended,
        )?;
        let projector =
            TauriExecutionProjector::without_app(repository.clone(), Some(runtime.clone()));
        let execution_id = "run-1:task-1:1:1";

        projector.emit(
            ExecEvent::subagent(
                "run-1",
                "task-1",
                execution_id,
                RuntimeEventKind::ToolStarted,
                serde_json::json!({
                    "call_id": "call-1",
                    "name": "read_file",
                    "args": {"path": "src/main.rs"},
                }),
            )
            .with_agent("explorer"),
        );
        projector.emit(ExecEvent::subagent(
            "run-1",
            "task-1",
            execution_id,
            RuntimeEventKind::ToolOutput,
            serde_json::json!({
                "call_id": "call-1",
                "name": "read_file",
                "channel": "stdout",
                "chunk": "main output",
            }),
        ));
        projector.emit(ExecEvent::subagent(
            "run-1",
            "task-1",
            execution_id,
            RuntimeEventKind::ToolCompleted,
            serde_json::json!({
                "call_id": "call-1",
                "name": "read_file",
                "success": true,
                "metadata": {"source": "stream"},
                "truncated": false,
            }),
        ));
        projector.emit(ExecEvent::subagent(
            "run-1",
            "task-1",
            execution_id,
            RuntimeEventKind::ToolCompleted,
            serde_json::json!({
                "call_id": "call-1",
                "name": "read_file",
                "result": "main output",
                "success": true,
            }),
        ));

        let summaries = repository.summaries_for_conversation("conversation-1");
        let completed = summaries
            .iter()
            .find(|summary| summary.call_id == "call-1")
            .ok_or_else(|| "missing completed tool summary".to_string())?;
        assert_eq!(completed.status, ToolExecutionStatus::Succeeded);
        assert_eq!(completed.run_id.as_deref(), Some("run-1"));
        let detail = repository.detail_manifest(&completed.detail_ref)?;
        assert_eq!(
            detail.metadata.get("source").map(String::as_str),
            Some("stream")
        );
        let output = repository.read_output(&completed.detail_ref, None, 1024)?;
        assert_eq!(
            output.chunks.first().map(|chunk| chunk.text.as_str()),
            Some("main output")
        );

        projector.emit(ExecEvent::subagent(
            "run-1",
            "task-1",
            execution_id,
            RuntimeEventKind::ToolStarted,
            serde_json::json!({
                "call_id": "call-2",
                "name": "shell",
                "args": {"command": "sleep 1"},
            }),
        ));
        projector.emit(ExecEvent::subagent(
            "run-1",
            "task-1",
            execution_id,
            RuntimeEventKind::Completed,
            serde_json::json!({}),
        ));

        let summaries = repository.summaries_for_conversation("conversation-1");
        let cancelled = summaries
            .iter()
            .find(|summary| summary.call_id == "call-2")
            .ok_or_else(|| "missing cancelled tool summary".to_string())?;
        assert_eq!(cancelled.status, ToolExecutionStatus::Cancelled);
        Ok(())
    }
}

#[cfg(test)]
mod foreground_turn_command_tests {
    use super::*;
    use echo_agent_app_core::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

    #[test]
    fn active_snapshot_returns_real_message_scope_without_product_conversation()
    -> Result<(), Box<dyn std::error::Error>> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Gui, "message:turn-1", "turn-1")?;
        let snapshot = select_active_chat_turn(&control, None)?
            .ok_or_else(|| "missing active snapshot".to_string())?;
        assert_eq!(snapshot.conversation_id, "message:turn-1");
        assert_eq!(snapshot.turn_id, "turn-1");
        lease.settle(echo_agent_app_core::chat_driver::TurnOutcome::Completed);
        Ok(())
    }

    #[test]
    fn active_snapshot_requires_scope_when_multiple_gui_turns_exist()
    -> Result<(), Box<dyn std::error::Error>> {
        let control = ForegroundTurnControl::default();
        let first = control.begin(ForegroundTurnSurface::Gui, "conversation-1", "turn-1")?;
        let second = control.begin(ForegroundTurnSurface::Gui, "conversation-2", "turn-2")?;
        assert!(matches!(
            select_active_chat_turn(&control, None),
            Err(IpcError::Validation(message))
                if message == "active_chat_turn_ambiguous:conversation_id_required"
        ));
        let snapshot = select_active_chat_turn(&control, Some("conversation-2"))?
            .ok_or_else(|| "missing exact active snapshot".to_string())?;
        assert_eq!(snapshot.turn_id, "turn-2");
        first.settle(echo_agent_app_core::chat_driver::TurnOutcome::Completed);
        second.settle(echo_agent_app_core::chat_driver::TurnOutcome::Completed);
        Ok(())
    }
}
