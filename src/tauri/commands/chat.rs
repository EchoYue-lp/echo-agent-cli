//! Tauri IPC commands for chat streaming.
//!
//! Uses `app.emit()` to stream the application-owned canonical chat envelope
//! to the frontend, replacing the WebSocket transport from the Axum server.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use echo_agent_app_core::chat_driver::ChatDriverEvent;
use echo_agent_app_core::chat_driver::ChatSink;
use echo_agent_app_core::chat_event_log::{
    ChatEventEnvelope, ChatEventLog, ChatSurface, bind_surface_chat_sink,
};
use echo_agent_app_core::tasks::task_runtime::executor::{ExecEvent, ExecEventScope};
use echo_agent_app_core::tool_execution::{ToolExecutionRepository, ToolExecutionSummary};
use echo_agent_app_core::tool_execution_projection::{
    ToolExecutionProjectionKind, ToolExecutionProjectionUpdate, ToolExecutionProjector,
};
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, MutexGuard as StdMutexGuard};
use tauri::Emitter;
use tokio::sync::oneshot;
use uuid::Uuid;

pub(crate) fn emit_chat_envelope(app: &tauri::AppHandle, envelope: &ChatEventEnvelope) -> bool {
    if let Err(error) = app.emit("chat://event", envelope) {
        tracing::warn!(%error, "failed to emit canonical chat journal event");
        return false;
    }
    true
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

fn deliver_hitl_request(
    waiting_status: &str,
    request: ChatDriverEvent,
    sink: &Arc<dyn ChatSink>,
) -> echo_agent::error::Result<()> {
    let status_delivered = sink.on_event(ChatDriverEvent::TurnStatus {
        status: waiting_status.to_string(),
    });
    let request_delivered = status_delivered && sink.on_event(request);
    if request_delivered {
        Ok(())
    } else {
        let _ = sink.on_event(ChatDriverEvent::TurnStatus {
            status: "failed".to_string(),
        });
        Err(echo_agent::error::ReactError::Other(
            "GUI human-loop request could not reach the durable surface; request cancelled"
                .to_string(),
        ))
    }
}

/// Tauri-based HumanLoopProvider — emits approval/input requests via Tauri events
/// and awaits responses through the shared PENDING_RESPONSES map.
pub(crate) struct TauriHumanLoopHandler {
    sink: Arc<dyn ChatSink>,
    pending: Arc<StdMutex<HashMap<String, PendingRequest>>>,
    message_key: String,
}

impl TauriHumanLoopHandler {
    pub(crate) fn new(sink: Arc<dyn ChatSink>, message_key: String) -> Self {
        Self {
            sink,
            pending: PENDING_RESPONSES.clone(),
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
        let sink = self.sink.clone();
        let pending = self.pending.clone();
        let message_key = self.message_key.clone();

        Box::pin(async move {
            tracing::debug!(
                request_id = %request_id,
                message_key = %message_key,
                "Tauri HITL request created"
            );

            match req.kind {
                echo_agent::human_loop::HumanLoopKind::Approval => {
                    let tool_name = req.tool_name.clone().unwrap_or_default();
                    let args = req.args.clone().unwrap_or(serde_json::Value::Null);
                    let event = ChatDriverEvent::ApprovalRequest {
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
                    deliver_hitl_request("waiting_approval", event, &sink)?;

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
                    let event = ChatDriverEvent::InputRequest {
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
                    deliver_hitl_request("waiting_input", event, &sink)?;

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
                    let event = ChatDriverEvent::SelectionRequest {
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
                    deliver_hitl_request("waiting_input", event, &sink)?;

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
    let (attachment_refs, mut staged_attachment_batch): (
        Vec<echo_agent_app_core::attachments::AttachmentRef>,
        Option<echo_agent_app_core::attachments::StagedAttachmentBatch>,
    ) = if saved_attachments.is_empty() {
        (Vec::new(), None)
    } else {
        let uploads_dir = echo_agent_app_core::attachments::resolve_uploads_dir(ws_root.as_deref());
        let saved =
            echo_agent_app_core::attachments::save_attachments(&saved_attachments, &uploads_dir)
                .map_err(|error| {
                    IpcError::Validation(format!("Failed to stage attachments: {error}"))
                })?;
        // Build refs (path + name + mime) for binding to the run so plan-level
        // subagents can rebuild the multimodal message later, and so the
        // PreparedUserTurn can re-read them for inline delivery.
        let refs = saved
            .iter()
            .map(|(path, att)| {
                echo_agent_app_core::attachments::AttachmentRef::from_saved(path.clone(), att)
            })
            .collect();
        let batch = echo_agent_app_core::attachments::StagedAttachmentBatch::from_saved(&saved);
        (refs, Some(batch))
    };
    if !attachment_refs.is_empty() {
        tracing::info!(
            count = attachment_refs.len(),
            "send_chat_message: multimodal message with attachments"
        );
    }

    let message_key = message_key.unwrap_or_else(|| Uuid::new_v4().to_string());
    let sink = tauri_chat_sink(
        app.clone(),
        message_key.clone(),
        conversation_id.clone(),
        state.app_state.storage.tool_executions.clone(),
        state.app_state.storage.chat_events.clone(),
    );

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
        if !sink.on_event(ChatDriverEvent::Interrupt {
            run_id: existing.run_id.clone(),
            goal: existing.goal.clone(),
            new_message: message.clone(),
        }) {
            return Err(IpcError::Internal(
                "failed to persist the interrupt prompt".to_string(),
            ));
        }
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
        .begin_conversation_turn_owned(
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Gui,
            &active_turn_key,
            message_key.clone(),
        )
        .await
        .map_err(|error| match error {
            echo_agent_app_core::conversation_deletion::ConversationDeletionError::Foreground(
                echo_agent_app_core::foreground_turn::ForegroundTurnError::Busy {
                    active_turn_id,
                    ..
                },
            ) => IpcError::Validation(format!("chat_turn_busy:{active_turn_id}")),
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
        sink.clone(),
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
    // Build the GUI sink + per-turn resources, then drive the whole turn
    // (normal reply AND any complex runs the agent autonomously spins up via
    // create_complex_task) through the single shared `drive_chat` entry. The
    // agent decides complexity itself (Phase B3) — no code route pre-judgment.
    // Signal the chat-turn lifecycle so the GUI shows the spinner / terminal
    // badge. Ordinary chat turns are not TaskRuntime runs.
    let _ = sink.on_event(ChatDriverEvent::TurnStatus {
        status: "running".to_string(),
    });

    let agent_handle_clone = agent_handle.clone();
    // Build the prepared turn (instruction + input resources, with long pastes
    // spilled to user-input artifacts). Replaces the old
    // (message, multimodal_message) pair handed to drive_chat.
    let spill_dir =
        echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(ws_root.as_deref());
    let prepared_turn = match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::prepared_turn::UserTurnInput {
            text: &message,
            attachments: &attachment_refs,
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
            let cleanup = staged_attachment_batch
                .take()
                .and_then(|batch| batch.rollback().err());
            let cleanup_suffix = cleanup
                .map(|error| format!("; staged attachment cleanup failed: {error}"))
                .unwrap_or_default();
            return Err(IpcError::Validation(format!(
                "failed to prepare user turn: {e}{cleanup_suffix}"
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
        interaction_mode,
        review_integration: state.app_state.review_integration.clone(),
        layer_manager: None,
        memory_generation: None,
        human_loop_provider: Some(hitl_handler),
    });
    if let Some(batch) = staged_attachment_batch.take() {
        batch.commit();
    }
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
        .map(|snapshot| snapshot.active_turn_id)
        .ok_or_else(|| IpcError::Validation("no active chat turn".to_string()))?;
    let saved_attachments = attachments.unwrap_or_default();
    let ws_root = state.app_state.current_workspace().await.map(|ws| ws.root);
    let uploads_dir = echo_agent_app_core::attachments::resolve_uploads_dir(ws_root.as_deref());
    let saved =
        echo_agent_app_core::attachments::save_attachments(&saved_attachments, &uploads_dir)
            .map_err(|error| {
                IpcError::Validation(format!("Failed to stage attachments: {error}"))
            })?;
    let mut staged_attachment_batch =
        Some(echo_agent_app_core::attachments::StagedAttachmentBatch::from_saved(&saved));
    let attachment_refs: Vec<_> = saved
        .iter()
        .map(|(path, att)| {
            echo_agent_app_core::attachments::AttachmentRef::from_saved(path.clone(), att)
        })
        .collect();
    let spill_dir =
        echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(ws_root.as_deref());
    let prepared = match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::prepared_turn::UserTurnInput {
            text: &message,
            attachments: &attachment_refs,
            spill_dir: &spill_dir,
            conversation_id: Some(&conversation_id),
            turn_id: Some(&expected_turn_id),
        },
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            let cleanup = staged_attachment_batch
                .take()
                .and_then(|batch| batch.rollback().err());
            let suffix = cleanup
                .map(|cleanup| format!("; staged attachment cleanup failed: {cleanup}"))
                .unwrap_or_default();
            return Err(IpcError::Validation(format!("{error}{suffix}")));
        }
    };
    if let Some(batch) = staged_attachment_batch.take() {
        batch.commit();
    }
    let steer_message = match prepared.to_message() {
        Ok(message) => message,
        Err(error) => {
            let cleanup = prepared.cleanup_resources(&spill_dir).err();
            let suffix = cleanup
                .map(|cleanup| format!("; prepared artifact cleanup failed: {cleanup}"))
                .unwrap_or_default();
            return Err(IpcError::Validation(format!("{error}{suffix}")));
        }
    };
    let agent_execution = match state.app_state.connection.agent_for(&conversation_id).await {
        Ok(execution) => execution,
        Err(error) => {
            let cleanup = prepared.cleanup_resources(&spill_dir).err();
            let suffix = cleanup
                .map(|cleanup| format!("; prepared artifact cleanup failed: {cleanup}"))
                .unwrap_or_default();
            return Err(IpcError::Validation(format!("{error}{suffix}")));
        }
    };
    let agent = agent_execution.agent();
    let result = agent
        .steer_input(Some(&expected_turn_id), steer_message)
        .await;
    match result {
        Ok(turn_id) => Ok(serde_json::json!({
            "kind": "accepted",
            "turn_id": turn_id,
        })),
        Err(error) => {
            let cleanup = prepared
                .cleanup_resources(&spill_dir)
                .err()
                .map(|cleanup| cleanup.to_string());
            let suffix = cleanup
                .as_ref()
                .map(|cleanup| format!("; prepared artifact cleanup failed: {cleanup}"))
                .unwrap_or_default();
            match error {
                echo_agent::agent::TurnSteerError::NotSteerable { turn_id } => {
                    Ok(serde_json::json!({
                        "kind": "not_steerable",
                        "turn_id": turn_id,
                        "cleanup_error": cleanup,
                    }))
                }
                echo_agent::agent::TurnSteerError::NoActiveTurn => Ok(serde_json::json!({
                    "kind": "no_active_turn",
                    "cleanup_error": cleanup,
                })),
                echo_agent::agent::TurnSteerError::TurnMismatch { expected, actual } => {
                    Ok(serde_json::json!({
                    "kind": "turn_mismatch",
                    "expected": expected,
                    "actual": actual,
                    "cleanup_error": cleanup,
                    }))
                }
                error => Err(IpcError::Validation(format!("{error}{suffix}"))),
            }
        }
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

/// Replay the canonical ordered journal after the WebView's last applied cursor.
#[tauri::command]
pub fn replay_chat_events(
    state: tauri::State<'_, TauriState>,
    conversation_id: Option<String>,
    message_key: Option<String>,
    after_cursor: Option<u64>,
) -> Result<echo_agent_app_core::chat_event_log::ChatEventReplay, IpcError> {
    let turn_id = message_key
        .or_else(|| conversation_id.clone())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            IpcError::Validation(
                "conversation_id or message_key is required for chat replay".to_string(),
            )
        })?;
    let retained = state
        .app_state
        .storage
        .chat_events
        .replay(conversation_id.as_deref(), &turn_id, 0)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    ToolExecutionProjector::new(
        state.app_state.storage.tool_executions.clone(),
        state.app_state.tasks.runtime.clone(),
    )
    .rebuild_from_retained(&retained.events)
    .map_err(|error| IpcError::Internal(error.to_string()))?;
    let cursor = after_cursor.unwrap_or(0);
    if cursor == 0 {
        Ok(retained)
    } else {
        state
            .app_state
            .storage
            .chat_events
            .replay(conversation_id.as_deref(), &turn_id, cursor)
            .map_err(|error| IpcError::Internal(error.to_string()))
    }
}

/// Cancel an active chat stream.
#[tauri::command]
pub async fn cancel_chat(
    state: tauri::State<'_, TauriState>,
    conversation_id: String,
    root_turn_id: String,
) -> Result<serde_json::Value, IpcError> {
    let waiter = state
        .app_state
        .session
        .foreground_turns
        .request_root_cancel(
            echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Gui,
            &conversation_id,
            &root_turn_id,
        )
        .map_err(|error| IpcError::Validation(error.to_string()))?;

    // Reject pending HITL before waiting so parked execution can reach its
    // terminal outcome. Ownership remains registered until that settlement.
    cancel_pending_hitl(Some(&root_turn_id), "cancelled by user").await;
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
    projector: Arc<ToolExecutionProjector>,
}

impl TauriExecutionProjector {
    pub(crate) fn new(
        app: tauri::AppHandle,
        tool_executions: Arc<ToolExecutionRepository>,
        task_runtime_store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    ) -> Self {
        Self {
            app: Some(app),
            projector: Arc::new(ToolExecutionProjector::new(
                tool_executions,
                task_runtime_store,
            )),
        }
    }

    pub(crate) fn emit(&self, event: ExecEvent) {
        match self.projector.project_execution_event(&event) {
            Ok(updates) => {
                self.emit_updates(&updates);
            }
            Err(error) => {
                tracing::error!(%error, run_id = %event.run_id, event = ?event.event, "failed to project canonical TaskRuntime tool event");
            }
        }
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
            projector: Arc::new(ToolExecutionProjector::new(
                tool_executions,
                task_runtime_store,
            )),
        }
    }

    fn emit_updates(&self, updates: &[ToolExecutionProjectionUpdate]) -> bool {
        let Some(app) = self.app.as_ref() else {
            return true;
        };
        updates.iter().all(|update| {
            emit_tool_execution_summary(
                app,
                match update.kind {
                    ToolExecutionProjectionKind::Started => "started",
                    ToolExecutionProjectionKind::Finished => "finished",
                },
                &update.agent,
                &update.summary,
            )
        })
    }
}

/// GUI renderer behind the shared group-committed chat sink. The Tauri wire
/// receives the canonical application envelope unchanged; execution details
/// are secondary projections derived and committed by app-core first.
///
/// This is the GUI equivalent of the TUI/channel `ChatSink`: the whole chat
/// turn (normal reply + any complex runs the agent autonomously spins up via
/// `create_complex_task`) flows through one unified `drive_chat`.
struct TauriChatSink {
    app: Option<tauri::AppHandle>,
    emit_envelope: Arc<dyn Fn(&ChatEventEnvelope) -> bool + Send + Sync>,
    emit_tool_projection: Arc<dyn Fn(&ToolExecutionProjectionUpdate) -> bool + Send + Sync>,
}

pub(crate) fn tauri_chat_sink(
    app: tauri::AppHandle,
    message_key: String,
    conversation_id: Option<String>,
    tool_executions: Arc<echo_agent_app_core::tool_execution::ToolExecutionRepository>,
    chat_events: Arc<ChatEventLog>,
) -> Arc<dyn ChatSink> {
    let envelope_app = app.clone();
    let projection_app = app.clone();
    let renderer = Arc::new(TauriChatSink {
        app: Some(app),
        emit_envelope: Arc::new(move |envelope| emit_chat_envelope(&envelope_app, envelope)),
        emit_tool_projection: Arc::new(move |update| {
            let event_name = match update.kind {
                ToolExecutionProjectionKind::Started => "started",
                ToolExecutionProjectionKind::Finished => "finished",
            };
            emit_tool_execution_summary(&projection_app, event_name, &update.agent, &update.summary)
        }),
    });
    bind_surface_chat_sink(
        ChatSurface::Gui,
        renderer,
        chat_events,
        tool_executions,
        conversation_id,
        message_key,
    )
}

impl TauriChatSink {
    #[cfg(test)]
    fn without_app(emit_envelope: Arc<dyn Fn(&ChatEventEnvelope) -> bool + Send + Sync>) -> Self {
        Self::without_app_with_projection(emit_envelope, Arc::new(|_| true))
    }

    #[cfg(test)]
    fn without_app_with_projection(
        emit_envelope: Arc<dyn Fn(&ChatEventEnvelope) -> bool + Send + Sync>,
        emit_tool_projection: Arc<dyn Fn(&ToolExecutionProjectionUpdate) -> bool + Send + Sync>,
    ) -> Self {
        Self {
            app: None,
            emit_envelope,
            emit_tool_projection,
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
    fn on_event(&self, _event: ChatDriverEvent) -> bool {
        tracing::error!("TauriChatSink received an event before the ordered chat journal boundary");
        false
    }

    fn on_journaled_event(&self, envelope: ChatEventEnvelope) -> bool {
        if let ChatDriverEvent::Execution(event) = &envelope.payload
            && let Some(app) = self.app.as_ref()
        {
            emit_tauri_execution_event(app, event.clone());
        }
        (self.emit_envelope)(&envelope)
    }

    fn on_tool_execution_projection(&self, update: &ToolExecutionProjectionUpdate) -> bool {
        (self.emit_tool_projection)(update)
    }
}

#[cfg(test)]
mod chat_sink_contract_tests {
    use super::*;
    use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity, ToolInvocation};
    use echo_agent::tools::ToolResult;
    use echo_agent_app_core::chat_event_log::ChatEventRetention;
    use echo_agent_app_core::tool_execution::ToolExecutionStatus;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> std::io::Result<Self> {
            let path =
                std::env::temp_dir().join(format!("eko-tauri-chat-contract-{}", Uuid::new_v4()));
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
                eprintln!("failed to clean Tauri chat contract directory: {error}");
            }
        }
    }

    fn invocation() -> ToolInvocation {
        ToolInvocation {
            requested_name: "shell".to_string(),
            requested_args: serde_json::json!({"command": "printf requested"}),
            name: "sandbox_shell".to_string(),
            args: serde_json::json!({"command": "printf effective"}),
            rewrites: Vec::new(),
        }
    }

    #[test]
    fn tauri_renderer_forwards_the_exact_journal_envelope() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = TestDir::new()?;
        let repository = Arc::new(ToolExecutionRepository::open(temp.path().join("tools"))?);
        let log = Arc::new(ChatEventLog::open(
            temp.path().join("chat"),
            ChatEventRetention::default(),
        )?);
        let captured = Arc::new(StdMutex::new(None::<serde_json::Value>));
        let captured_for_emit = captured.clone();
        let renderer: Arc<dyn ChatSink> = Arc::new(TauriChatSink::without_app(Arc::new(
            move |envelope| match serde_json::to_value(envelope) {
                Ok(value) => {
                    *lock_std(&captured_for_emit, "captured Tauri chat envelope") = Some(value);
                    true
                }
                Err(_) => false,
            },
        )));
        let sink = bind_surface_chat_sink(
            ChatSurface::Gui,
            renderer,
            log.clone(),
            repository,
            Some("conversation-wire".to_string()),
            "root-message",
        );
        let identity = EventIdentity::for_chat(
            Some("conversation-wire".to_string()),
            "turn-wire",
            "root-message",
            None,
        )?;
        let event = EventEnvelope::new(
            &identity,
            1,
            None,
            AgentEvent::ToolCall {
                call_id: "call-wire".to_string(),
                invocation: invocation(),
            },
        )?;

        assert!(sink.on_event(ChatDriverEvent::Agent(Box::new(event))));
        let emitted = lock_std(&captured, "captured Tauri chat envelope")
            .clone()
            .ok_or_else(|| std::io::Error::other("Tauri renderer emitted no envelope"))?;
        let replay = log.replay(Some("conversation-wire"), "ignored", 0)?;
        let persisted = replay
            .events
            .first()
            .ok_or_else(|| std::io::Error::other("journal replay returned no envelope"))?;
        assert_eq!(emitted, serde_json::to_value(persisted)?);
        Ok(())
    }

    #[test]
    fn failed_tool_projection_emit_preserves_terminal_hydration_and_exact_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TestDir::new()?;
        let tool_root = temp.path().join("tools");
        let repository = Arc::new(ToolExecutionRepository::open(&tool_root)?);
        let log = Arc::new(ChatEventLog::open(
            temp.path().join("chat"),
            ChatEventRetention::default(),
        )?);
        let projection_count = Arc::new(AtomicUsize::new(0));
        let observed_projection_count = projection_count.clone();
        let envelope_count = Arc::new(AtomicUsize::new(0));
        let observed_envelope_count = envelope_count.clone();
        let renderer: Arc<dyn ChatSink> = Arc::new(TauriChatSink::without_app_with_projection(
            Arc::new(move |_| {
                observed_envelope_count.fetch_add(1, Ordering::SeqCst);
                true
            }),
            Arc::new(move |_| observed_projection_count.fetch_add(1, Ordering::SeqCst) == 0),
        ));
        let sink = bind_surface_chat_sink(
            ChatSurface::Gui,
            renderer,
            log,
            repository.clone(),
            Some("conversation-1".to_string()),
            "message-1",
        );
        let identity = EventIdentity::for_chat(
            Some("conversation-1".to_string()),
            "turn-1",
            "message-1",
            None,
        )?;
        let call = EventEnvelope::new(
            &identity,
            1,
            None,
            AgentEvent::ToolCall {
                call_id: "call-1".to_string(),
                invocation: invocation(),
            },
        )?;
        assert!(sink.on_event(ChatDriverEvent::Agent(Box::new(call))));
        let result = EventEnvelope::new(
            &identity,
            2,
            None,
            AgentEvent::ToolResult {
                call_id: "call-1".to_string(),
                name: "sandbox_shell".to_string(),
                result: ToolResult::success("complete output")
                    .with_meta("duration_ms", "10")
                    .with_truncated(true),
            },
        )?;
        assert!(!sink.on_event(ChatDriverEvent::Agent(Box::new(result))));
        assert_eq!(projection_count.load(Ordering::SeqCst), 2);
        assert_eq!(envelope_count.load(Ordering::SeqCst), 1);

        let detail_ref = repository
            .summaries_for_conversation("conversation-1")
            .into_iter()
            .find(|summary| summary.call_id == "call-1")
            .ok_or_else(|| std::io::Error::other("tool result projection missing"))?
            .detail_ref;
        drop(sink);
        drop(repository);

        let rebound = ToolExecutionRepository::open(tool_root)?;
        let summary = rebound
            .summaries_for_conversation("conversation-1")
            .into_iter()
            .find(|summary| summary.call_id == "call-1")
            .ok_or_else(|| std::io::Error::other("tool result did not survive reopen"))?;
        assert_eq!(summary.status, ToolExecutionStatus::Succeeded);
        let detail = rebound.detail_manifest(&detail_ref)?;
        let result = detail
            .result
            .as_ref()
            .ok_or_else(|| std::io::Error::other("canonical result missing after reopen"))?;
        assert!(result.truncated);
        assert_eq!(
            result.metadata.get("duration_ms").map(String::as_str),
            Some("10")
        );
        Ok(())
    }

    #[test]
    fn retained_replay_rebuilds_tool_detail_idempotently() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = TestDir::new()?;
        let log = ChatEventLog::open(temp.path().join("chat"), ChatEventRetention::default())?;
        let identity = EventIdentity::for_chat(
            Some("conversation-replay".to_string()),
            "turn-replay",
            "root-replay",
            None,
        )?;
        for (sequence, event) in [
            (
                1,
                AgentEvent::ToolCall {
                    call_id: "call-replay".to_string(),
                    invocation: invocation(),
                },
            ),
            (
                2,
                AgentEvent::ToolResult {
                    call_id: "call-replay".to_string(),
                    name: "sandbox_shell".to_string(),
                    result: ToolResult::success("replayed output")
                        .with_meta("artifact_path", "/tmp/replayed-output.txt"),
                },
            ),
        ] {
            let envelope = EventEnvelope::new(&identity, sequence, None, event)?;
            log.append(
                Some("conversation-replay"),
                "root-replay",
                ChatDriverEvent::Agent(Box::new(envelope)),
            )?;
        }

        let repository = Arc::new(ToolExecutionRepository::open(temp.path().join("tools"))?);
        let replay = log.replay(Some("conversation-replay"), "ignored", 0)?;
        let projector = ToolExecutionProjector::new(repository.clone(), None);
        projector.rebuild_from_retained(&replay.events)?;
        projector.rebuild_from_retained(&replay.events)?;

        let summaries = repository.summaries_for_conversation("conversation-replay");
        assert_eq!(summaries.len(), 1);
        let summary = summaries
            .first()
            .ok_or_else(|| std::io::Error::other("replayed tool detail missing"))?;
        assert_eq!(summary.status, ToolExecutionStatus::Succeeded);
        let detail = repository.detail_manifest(&summary.detail_ref)?;
        let result = detail
            .result
            .ok_or_else(|| std::io::Error::other("replayed canonical result missing"))?;
        assert_eq!(result.output, "replayed output");
        assert_eq!(
            result.metadata.get("artifact_path").map(String::as_str),
            Some("/tmp/replayed-output.txt")
        );
        Ok(())
    }

    #[test]
    fn failed_durable_hitl_delivery_is_rejected_before_waiting()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TestDir::new()?;
        let repository = Arc::new(ToolExecutionRepository::open(temp.path().join("tools"))?);
        let log = Arc::new(ChatEventLog::open(
            temp.path().join("chat"),
            ChatEventRetention::default(),
        )?);
        let renderer: Arc<dyn ChatSink> = Arc::new(TauriChatSink::without_app(Arc::new(|_| false)));
        let sink = bind_surface_chat_sink(
            ChatSurface::Gui,
            renderer,
            log.clone(),
            repository,
            Some("conversation-hitl".to_string()),
            "root-hitl",
        );

        let result = deliver_hitl_request(
            "waiting_input",
            ChatDriverEvent::InputRequest {
                request_id: "request-hitl".to_string(),
                prompt: "input".to_string(),
            },
            &sink,
        );
        assert!(result.is_err());

        let replay = log.replay(Some("conversation-hitl"), "ignored", 0)?;
        let statuses = replay
            .events
            .iter()
            .filter_map(|envelope| match &envelope.payload {
                ChatDriverEvent::TurnStatus { status } => Some(status.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(statuses, vec!["waiting_input", "failed"]);
        assert!(
            replay
                .events
                .iter()
                .all(|envelope| !matches!(&envelope.payload, ChatDriverEvent::InputRequest { .. }))
        );
        Ok(())
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
    use echo_agent_app_core::tasks::task_runtime::types::RuntimeEventKind;
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
    fn task_runtime_tools_preserve_canonical_result_and_unknown_orphan_status()
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
                    "invocation": {
                        "requested_name": "read_file",
                        "requested_args": {"path": "src/main.rs"},
                        "name": "read_file",
                        "args": {"path": "src/main.rs"},
                        "rewrites": [],
                    },
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
                "result": {
                    "kind": {"kind_type": "text"},
                    "success": true,
                    "output": "main output",
                    "error": null,
                    "data": null,
                    "truncated": false,
                    "mime_type": "text/plain",
                    "metadata": {"source": "canonical-result"},
                },
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
            detail
                .result
                .as_ref()
                .and_then(|result| result.metadata.get("source"))
                .map(String::as_str),
            Some("canonical-result")
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
                "invocation": {
                    "requested_name": "shell",
                    "requested_args": {"command": "sleep 1"},
                    "name": "shell",
                    "args": {"command": "sleep 1"},
                    "rewrites": [],
                },
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
        let orphaned = summaries
            .iter()
            .find(|summary| summary.call_id == "call-2")
            .ok_or_else(|| "missing orphaned tool summary".to_string())?;
        assert_eq!(orphaned.status, ToolExecutionStatus::Unknown);
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
        assert_eq!(snapshot.root_turn_id, "turn-1");
        assert_eq!(snapshot.active_turn_id, "turn-1");
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
        assert_eq!(snapshot.root_turn_id, "turn-2");
        assert_eq!(snapshot.active_turn_id, "turn-2");
        first.settle(echo_agent_app_core::chat_driver::TurnOutcome::Completed);
        second.settle(echo_agent_app_core::chat_driver::TurnOutcome::Completed);
        Ok(())
    }
}
