//! Tauri IPC commands for chat streaming.
//!
//! Uses `app.emit()` to stream the application-owned canonical chat envelope
//! to the frontend, replacing the WebSocket transport from the Axum server.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use echo_agent_app_core::api::chat_driver::ChatDriverEvent;
use echo_agent_app_core::api::chat_driver::ChatSink;
use echo_agent_app_core::api::chat_event_log::{
    ChatEventEnvelope, ChatEventLog, ChatSurface, bind_surface_chat_sink,
};
#[cfg(test)]
use echo_agent_app_core::api::conversation_input::ConversationInputPhase;
use echo_agent_app_core::api::conversation_input::{
    ConversationInputAddress, ConversationInputAttempt, ConversationInputIdentity,
    ConversationInputProjection, ConversationInputReceipt, ConversationInputSource,
    stable_scoped_input_id,
};
use echo_agent_app_core::api::subagent_event_projection::JournaledExecutionProjector;
use echo_agent_app_core::api::tasks::task_runtime::executor::ExecEvent;
use echo_agent_app_core::api::tool_execution::{ToolExecutionRepository, ToolExecutionSummary};
use echo_agent_app_core::api::tool_execution_projection::{
    ToolExecutionProjectionKind, ToolExecutionProjectionUpdate, ToolExecutionProjector,
};
use futures::future::BoxFuture;
use serde::Deserialize;
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

fn conversation_input_address(
    workspace_id: &str,
    conversation_id: &str,
) -> ConversationInputAddress {
    ConversationInputAddress {
        workspace_id: workspace_id.to_string(),
        conversation_id: conversation_id.to_string(),
    }
}

fn conversation_input_attempt(
    projection: &ConversationInputProjection,
) -> Result<ConversationInputAttempt, IpcError> {
    let receipt = &projection.receipt;
    let attempt = projection.active_attempt.clone().ok_or_else(|| {
        IpcError::Internal("conversation input active attempt is missing".to_string())
    })?;
    if attempt.identity != receipt.identity
        || receipt.attempt != Some(attempt.attempt)
        || receipt.attempt_id.as_deref() != Some(attempt.attempt_id.as_str())
        || receipt.turn_id.as_deref() != Some(attempt.turn_id.as_str())
    {
        return Err(IpcError::Internal(
            "conversation input active attempt does not match its receipt".to_string(),
        ));
    }
    Ok(attempt)
}

fn emit_conversation_input_lifecycle_after(
    app: &tauri::AppHandle,
    log: &ChatEventLog,
    address: &ConversationInputAddress,
    after_cursor: u64,
) -> Result<(), IpcError> {
    let replay = log
        .replay(
            &address.workspace_id,
            Some(&address.conversation_id),
            &address.conversation_id,
            after_cursor,
        )
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let mut failed = 0usize;
    for envelope in replay.events {
        if matches!(&envelope.payload, ChatDriverEvent::InputLifecycle(_))
            && !emit_chat_envelope(app, &envelope)
        {
            failed = failed.saturating_add(1);
        }
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(IpcError::Internal(format!(
            "failed to emit {failed} durable conversation input event(s)"
        )))
    }
}

fn record_gui_transport_debt(operation: &'static str, error: impl std::fmt::Display) {
    tracing::error!(%error, operation, "GUI transport debt recorded after durable commit");
}

fn emit_gui_turn_status(sink: &Arc<dyn ChatSink>, status: &str) -> bool {
    let delivered = sink.on_event(ChatDriverEvent::TurnStatus {
        status: status.to_string(),
    });
    if !delivered {
        record_gui_transport_debt("turn_status", format!("failed to emit {status}"));
    }
    delivered
}

async fn settle_gui_input_attempt(
    service: &echo_agent_app_core::api::conversation_input::ConversationInputService,
    attempt: &ConversationInputAttempt,
    outcome: &echo_agent_app_core::api::chat_driver::TurnOutcome,
) -> Result<u64, String> {
    let receipt = service
        .settle_attempt(attempt, outcome)
        .await
        .map_err(|error| error.to_string())?;
    Ok(receipt.queue_revision.saturating_sub(1))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendChatMessageRequest {
    workspace_id: String,
    message: String,
    conversation_id: Option<String>,
    message_key: Option<String>,
    attachments: Option<Vec<echo_agent_app_core::api::types::AttachmentData>>,
    input_identity: Option<ConversationInputIdentity>,
    expected_queue_revision: Option<u64>,
}

pub(crate) struct ExecutionEventProjection<'a> {
    pub workspace_id: &'a str,
    pub conversation_id: &'a str,
    pub run_id: &'a str,
    pub kind: &'a str,
    pub event: &'a str,
    pub agent: &'a str,
    pub subagent_run_id: &'a str,
    pub payload: serde_json::Value,
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
    projection: ExecutionEventProjection<'_>,
) {
    let mut map = serde_json::Map::new();
    map.insert("workspace_id".into(), projection.workspace_id.into());
    map.insert("conversation_id".into(), projection.conversation_id.into());
    map.insert("kind".into(), projection.kind.into());
    if projection.kind == "subagent" {
        // Fall back to "main" only when a caller genuinely has no task_id
        // (shouldn't happen for kind="subagent", but guards against empty string).
        let id = if projection.subagent_run_id.is_empty() {
            "main"
        } else {
            projection.subagent_run_id
        };
        map.insert("subagent_run_id".into(), id.into());
        map.insert("agent".into(), projection.agent.into());
    }
    map.insert("run_id".into(), projection.run_id.into());
    map.insert("event".into(), projection.event.into());
    if let serde_json::Value::Object(fields) = projection.payload {
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
        ExecutionEventProjection {
            workspace_id: &summary.workspace_id,
            conversation_id: summary.conversation_id.as_deref().unwrap_or(""),
            run_id: summary.run_id.as_deref().unwrap_or(""),
            kind: "tool",
            event,
            agent,
            subagent_run_id: "",
            payload,
        },
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
    request: SendChatMessageRequest,
) -> Result<serde_json::Value, IpcError> {
    let SendChatMessageRequest {
        workspace_id,
        message,
        conversation_id,
        message_key,
        attachments,
        input_identity,
        expected_queue_revision,
    } = request;
    let conversation_id = conversation_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| IpcError::Validation("conversation_id is required".to_string()))?;
    let scoped_runtime = state
        .app_state
        .chat_runtime_for_scope(&workspace_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let message_key = message_key.unwrap_or_else(|| Uuid::new_v4().to_string());
    let address = conversation_input_address(&workspace_id, &conversation_id);
    let input_service = state.app_state.conversation_inputs();
    let frontier_before = input_service
        .list(&address)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let (input_identity, input_frontier) = if let Some(identity) = input_identity {
        if identity.address != address {
            return Err(IpcError::Validation(
                "queued input belongs to another conversation".to_string(),
            ));
        }
        let expected = expected_queue_revision.ok_or_else(|| {
            IpcError::Validation("expected_queue_revision is required".to_string())
        })?;
        if frontier_before.queue_revision != expected {
            return Ok(serde_json::json!({
                "kind": "queued",
                "workspace_id": workspace_id,
                "conversation_id": conversation_id,
                "input_id": identity.input_id,
            }));
        }
        (identity, frontier_before)
    } else {
        let input_id = stable_scoped_input_id(&address, ConversationInputSource::Gui, &message_key)
            .map_err(|error| IpcError::Validation(error.to_string()))?;
        let receipt = input_service
            .submit(
                address.clone(),
                input_id,
                message,
                attachments.unwrap_or_default(),
            )
            .await
            .map_err(|error| IpcError::Validation(error.to_string()))?;
        if let Err(error) = emit_conversation_input_lifecycle_after(
            &app,
            state.app_state.storage.chat_events.as_ref(),
            &address,
            frontier_before.queue_revision,
        ) {
            record_gui_transport_debt("persisted_input_lifecycle", error);
        }
        let frontier = input_service
            .list(&address)
            .await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        (receipt.identity, frontier)
    };
    let queued_input = input_frontier
        .items
        .iter()
        .find(|item| item.receipt.identity == input_identity)
        .cloned()
        .ok_or_else(|| IpcError::Validation("queued input is no longer pending".to_string()))?;
    let dispatch_queue_revision = input_frontier.queue_revision;
    let ws_root = scoped_runtime.execution_scope().root().to_path_buf();
    // ── Persist attachments + build multimodal message (if any) ──────────
    // The frontend base64-encodes uploads; we write them to a per-workspace
    // uploads dir and rebuild a `Message` with the right ContentParts so the
    // LLM sees images/files via the unified PreparedUserTurn (instruction + input
    // resources). Attachments are persisted first, then converted to refs; the
    // turn's to_message() rebuilds the multimodal Message from disk (the refs
    // path), so the in-memory `build_message` helper is no longer used here.
    let saved_attachments = queued_input.payload.attachments.clone();
    let (attachment_refs, mut staged_attachment_batch): (
        Vec<echo_agent_app_core::api::attachments::AttachmentRef>,
        Option<echo_agent_app_core::api::attachments::StagedAttachmentBatch>,
    ) = if saved_attachments.is_empty() {
        (Vec::new(), None)
    } else {
        let uploads_dir =
            echo_agent_app_core::api::attachments::resolve_uploads_dir(Some(&ws_root));
        let saved = echo_agent_app_core::api::attachments::save_attachments(
            &saved_attachments,
            &uploads_dir,
        )
        .map_err(|error| IpcError::Validation(format!("Failed to stage attachments: {error}")))?;
        // Build refs (path + name + mime) for binding to the run so plan-level
        // subagents can rebuild the multimodal message later, and so the
        // PreparedUserTurn can re-read them for inline delivery.
        let refs = saved
            .iter()
            .map(|(path, att)| {
                echo_agent_app_core::api::attachments::AttachmentRef::from_saved(path.clone(), att)
            })
            .collect();
        let batch =
            echo_agent_app_core::api::attachments::StagedAttachmentBatch::from_saved(&saved);
        (refs, Some(batch))
    };
    if !attachment_refs.is_empty() {
        tracing::info!(
            count = attachment_refs.len(),
            "send_chat_message: multimodal message with attachments"
        );
    }

    let sink = tauri_chat_sink(
        app.clone(),
        workspace_id.clone(),
        message_key.clone(),
        Some(conversation_id.clone()),
        state.app_state.storage.tool_executions.clone(),
        state.app_state.storage.chat_events.clone(),
    );

    // ── Interrupt detection ─────────────────────────────────────────────
    // If the same conversation already has an in-progress (Running/Paused)
    // run, do NOT start a new one. Instead, emit an InterruptPrompt event
    // so the GUI can ask the user what to do (resume / edit-and-resume /
    // abandon).
    let in_progress_run = if let Some(store) = scoped_runtime.task_runtime() {
        let conv_id = conversation_id.clone();
        echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeOperation::new(store)
            .run_store("load GUI in-progress TaskRun", move |store| {
                store.find_in_progress_run_by_conversation(&conv_id)
            })
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    if let Some(existing) = in_progress_run
        && matches!(
            existing.status,
            echo_agent_app_core::api::tasks::task_runtime::TaskRunStatus::Running
                | echo_agent_app_core::api::tasks::task_runtime::TaskRunStatus::Paused
        )
    {
        let conv_id = existing.conversation_id.clone();
        if !sink.on_event(ChatDriverEvent::Interrupt {
            run_id: existing.run_id.clone(),
            goal: existing.goal.clone(),
            new_message: queued_input.payload.text.clone(),
        }) {
            return Err(IpcError::Internal(
                "failed to persist the interrupt prompt".to_string(),
            ));
        }
        return Ok(serde_json::json!({
            "kind": "task_run_conflict",
            "workspace_id": workspace_id,
            "conversation_id": conv_id,
            "run_id": existing.run_id,
            "run_status": existing.status.as_str(),
            "goal": existing.goal,
            "new_message": queued_input.payload.text,
            "message_key": message_key,
            "input_id": input_identity.input_id,
        }));
    }

    let active_turn_key = conversation_id.clone();
    let foreground_lease = match scoped_runtime
        .begin_turn(
            &state.app_state.session.foreground_turns,
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
            &active_turn_key,
            message_key.clone(),
        )
        .await
    {
        Ok(lease) => lease,
        Err(
            echo_agent_app_core::api::conversation_deletion::ConversationDeletionError::Foreground(
                echo_agent_app_core::api::foreground_turn::ForegroundTurnError::Busy { .. },
            ),
        ) => {
            return Ok(serde_json::json!({
                "kind": "queued",
                "workspace_id": workspace_id,
                "conversation_id": conversation_id,
                "input_id": input_identity.input_id,
            }));
        }
        Err(error) => return Err(IpcError::Validation(error.to_string())),
    };
    let cancel_token = foreground_lease.cancellation_token();

    // Foreground admission must precede pool admission. Retain the pool
    // receipt in the spawned turn until the shared driver and HITL reset have
    // both settled, so workspace publication cannot race an issued handle.
    let pool_execution = scoped_runtime
        .agent_for(&conversation_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let agent_handle = pool_execution.agent();

    // Ensure stable cache_user_id for KVCache isolation (DeepSeek requires this
    // for prompt cache reuse across requests; without it, cache hit rate drops
    // to <1% because every request is treated as from a different user).
    // Persisted to ~/.eko/cache_user_id — generated once, reused forever.
    {
        let cache_id = echo_agent_app_core::api::infra::load_or_create_cache_user_id();
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
    let browser_approval_address = echo_agent_app_core::api::browser::BrowserApprovalAddress::new(
        workspace_id.clone(),
        active_turn_key.clone(),
    );
    let browser_approval_registration = state
        .browser_runtime
        .register_approval_provider(
            browser_approval_address,
            scoped_runtime.execution_scope().root().to_path_buf(),
            hitl_handler.clone(),
        )
        .await;

    // Build the GUI sink + per-turn resources, then drive the whole turn
    // (normal reply AND any complex runs the agent autonomously spins up via
    // create_complex_task) through the single shared `drive_chat` entry. The
    // agent decides complexity itself (Phase B3) — no code route pre-judgment.
    let agent_handle_clone = agent_handle.clone();
    // Build the prepared turn (instruction + input resources, with long pastes
    // spilled to user-input artifacts). Replaces the old
    // (message, multimodal_message) pair handed to drive_chat.
    let spill_dir =
        echo_agent_app_core::api::prepared_turn::resolve_user_input_spill_dir(Some(&ws_root));
    let prepared_turn = match echo_agent_app_core::api::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::api::prepared_turn::UserTurnInput {
            text: &queued_input.payload.text,
            attachments: &attachment_refs,
            spill_dir: &spill_dir,
            conversation_id: Some(&conversation_id),
            turn_id: Some(&message_key),
        },
    ) {
        Ok(turn) => turn,
        Err(e) => {
            tracing::warn!(error = %e, "failed to prepare user turn");
            let settlement_error = foreground_lease
                .settle_after_observers(echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                    echo_agent::error::AgentFailure::message("prepared_turn", e.to_string()),
                ))
                .await
                .err();
            let cleanup = staged_attachment_batch
                .take()
                .and_then(|batch| batch.rollback().err());
            let cleanup_suffix = settlement_error
                .map(|error| format!("; foreground settlement failed: {error}"))
                .into_iter()
                .chain(cleanup.map(|error| format!("; staged attachment cleanup failed: {error}")))
                .collect::<String>();
            return Err(IpcError::Validation(format!(
                "failed to prepare user turn: {e}{cleanup_suffix}"
            )));
        }
    };
    let res = std::sync::Arc::new(echo_agent_app_core::api::chat_resources::ChatResources {
        execution_scope: scoped_runtime.execution_scope().clone(),
        workspace_io_receipt: Some(scoped_runtime.workspace_io_receipt()),
        pool: scoped_runtime.pool(),
        store: scoped_runtime.task_runtime(),
        sink: sink.clone(),
        webhook_emitter: Some(state.app_state.webhook.emitter.clone()),
        conv_id: Some(active_turn_key.clone()),
        root_message_id: message_key.clone(),
        attachments: prepared_turn.inline_attachment_refs(),
        cancel: cancel_token.clone(),
        review_integration: scoped_runtime.review_integration(),
        memory_generation: None,
        human_loop_provider: Some(hitl_handler),
    });
    let started_input = match input_service
        .dispatch_selected(input_identity, dispatch_queue_revision, message_key.clone())
        .await
    {
        Ok(started) => started,
        Err(error) => {
            let settlement_error = foreground_lease
                .settle_after_observers(
                    echo_agent_app_core::api::chat_driver::TurnOutcome::Cancelled,
                )
                .await
                .err();
            browser_approval_registration.close().await;
            let prepared_cleanup = prepared_turn.cleanup_resources(&spill_dir).err();
            let staged_cleanup = staged_attachment_batch
                .take()
                .and_then(|batch| batch.rollback().err());
            let suffix = prepared_cleanup
                .map(|cleanup| format!("; prepared artifact cleanup failed: {cleanup}"))
                .into_iter()
                .chain(
                    staged_cleanup
                        .map(|cleanup| format!("; staged attachment cleanup failed: {cleanup}")),
                )
                .chain(
                    settlement_error
                        .map(|settlement| format!("; foreground settlement failed: {settlement}")),
                )
                .collect::<String>();
            return Err(IpcError::Validation(format!("{error}{suffix}")));
        }
    };
    if let Err(error) = emit_conversation_input_lifecycle_after(
        &app,
        state.app_state.storage.chat_events.as_ref(),
        &address,
        dispatch_queue_revision,
    ) {
        record_gui_transport_debt("attempt_started_lifecycle", error);
    }
    let input_attempt = conversation_input_attempt(&started_input)?;
    let observer_service = input_service.clone();
    let observer_attempt = input_attempt.clone();
    let observer_address = address.clone();
    let observer_app = app.clone();
    let observer_log = state.app_state.storage.chat_events.clone();
    let observer_after_cursor = started_input.receipt.queue_revision;
    let input_observer: echo_agent_app_core::api::chat_driver::InputReceiptObserver =
        Arc::new(move |receipt| {
            let service = observer_service.clone();
            let attempt = observer_attempt.clone();
            let address = observer_address.clone();
            let app = observer_app.clone();
            let log = observer_log.clone();
            Box::pin(async move {
                let observed = service
                    .observe_turn_input_through_drain(attempt, receipt)
                    .await
                    .map_err(|error| error.to_string());
                if let Err(error) = emit_conversation_input_lifecycle_after(
                    &app,
                    log.as_ref(),
                    &address,
                    observer_after_cursor,
                ) {
                    record_gui_transport_debt("initial_input_lifecycle", error);
                }
                observed.map(|_| ())
            })
        });
    // The durable attempt exists before the visible running projection.
    let _ = emit_gui_turn_status(&sink, "running");
    if let Some(batch) = staged_attachment_batch.take() {
        batch.commit();
    }
    let terminal_service = input_service.clone();
    let terminal_address = address.clone();
    let terminal_attempt = input_attempt;
    let durable_app = app.clone();
    let durable_log = state.app_state.storage.chat_events.clone();
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        // The prepared turn carries instruction + inline resources (images /
        // files re-read from disk via refs). Background runs created by
        // create_complex_task pick up attachments via ChatResources.attachments
        // (already bound above).
        let outcome =
            echo_agent_app_core::api::foreground_turn::drive_foreground_chat_with_ingress(
                foreground_lease,
                &agent_handle_clone,
                &prepared_turn,
                res,
                input_observer,
                move |outcome| {
                    let service = terminal_service.clone();
                    let address = terminal_address.clone();
                    let attempt = terminal_attempt.clone();
                    let app = durable_app.clone();
                    let log = durable_log.clone();
                    async move {
                        let after_cursor =
                            settle_gui_input_attempt(&service, &attempt, &outcome).await?;
                        if let Err(error) = emit_conversation_input_lifecycle_after(
                            &app,
                            log.as_ref(),
                            &address,
                            after_cursor,
                        ) {
                            record_gui_transport_debt("terminal_input_lifecycle", error);
                        }
                        Ok(())
                    }
                },
            )
            .await;
        let terminal_status = match &outcome {
            Ok(terminal_outcome) => {
                let status = terminal_outcome.status();
                // `drive_foreground_chat_with_ingress` publishes an outcome only
                // after exact durable input settlement and lease release.
                let _ = emit_gui_turn_status(&sink, status);
                status
            }
            Err(error) => {
                tracing::error!(%error, "GUI foreground terminal remains durable debt");
                "settlement_debt"
            }
        };
        if let Err(error) = &outcome {
            tracing::warn!(%error, "drive_chat chat turn errored");
        }
        agent_handle_clone
            .write_async(|agent| {
                Box::pin(async move {
                    let empty = Arc::new(echo_agent_app_core::api::hitl::HitlDispatcher::new());
                    agent.set_human_loop_provider_preserving_approvals(empty);
                })
            })
            .await;
        browser_approval_registration.close().await;
        drop(pool_execution);
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            status = %terminal_status,
            "Tauri chat turn finished (drive_chat)"
        );
    });

    Ok(serde_json::json!({
        "kind": "started",
        "success": true,
        "workspace_id": workspace_id,
        "conversation_id": conversation_id,
        "input_id": started_input.receipt.identity.input_id,
        "message_key": message_key,
        "root_turn_id": message_key,
        "active_turn_id": message_key,
    }))
}

/// Inject additional user input into the active foreground turn.
#[tauri::command]
pub async fn steer_chat_message(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    workspace_id: String,
    conversation_id: String,
    expected_active_turn_id: String,
    identity: ConversationInputIdentity,
    expected_queue_revision: u64,
) -> Result<ConversationInputReceipt, IpcError> {
    let address = conversation_input_address(&workspace_id, &conversation_id);
    if identity.address != address {
        return Err(IpcError::Validation(
            "queued input belongs to another conversation".to_string(),
        ));
    }
    let scoped_runtime = state
        .app_state
        .chat_runtime_for_scope(&workspace_id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let active = state
        .app_state
        .session
        .foreground_turns
        .snapshots_for_conversation_scoped(&workspace_id, &conversation_id)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let snapshot = active
        .first()
        .ok_or_else(|| IpcError::Validation("no active chat turn".to_string()))?;
    if snapshot.active_turn_id != expected_active_turn_id {
        return Err(IpcError::Validation(format!(
            "active chat turn mismatch: expected {expected_active_turn_id}, actual {}",
            snapshot.active_turn_id
        )));
    }
    let expected_turn_id = snapshot.active_turn_id.clone();
    let service = state.app_state.conversation_inputs();
    let frontier = service
        .list(&address)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    if frontier.queue_revision != expected_queue_revision {
        return Err(IpcError::Validation(
            "queued input frontier changed; refresh and retry".to_string(),
        ));
    }
    let queued = frontier
        .items
        .iter()
        .find(|item| item.receipt.identity == identity)
        .cloned()
        .ok_or_else(|| IpcError::Validation("queued input is no longer pending".to_string()))?;
    let saved_attachments = queued.payload.attachments.clone();
    let ws_root = scoped_runtime.execution_scope().root();
    let uploads_dir = echo_agent_app_core::api::attachments::resolve_uploads_dir(Some(ws_root));
    let saved = echo_agent_app_core::api::attachments::save_attachments(
        &saved_attachments,
        &uploads_dir,
    )
    .map_err(|error| IpcError::Validation(format!("Failed to stage attachments: {error}")))?;
    let mut staged_attachment_batch =
        Some(echo_agent_app_core::api::attachments::StagedAttachmentBatch::from_saved(&saved));
    let attachment_refs: Vec<_> = saved
        .iter()
        .map(|(path, att)| {
            echo_agent_app_core::api::attachments::AttachmentRef::from_saved(path.clone(), att)
        })
        .collect();
    let spill_dir =
        echo_agent_app_core::api::prepared_turn::resolve_user_input_spill_dir(Some(ws_root));
    let prepared = match echo_agent_app_core::api::prepared_turn::PreparedUserTurn::build(
        echo_agent_app_core::api::prepared_turn::UserTurnInput {
            text: &queued.payload.text,
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
    let steer_text = queued.payload.text.clone();
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
    let agent_execution = match scoped_runtime.agent_for(&conversation_id).await {
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
    let started = match service
        .dispatch_selected(identity, expected_queue_revision, expected_turn_id.clone())
        .await
    {
        Ok(started) => started,
        Err(error) => {
            let prepared_cleanup = prepared.cleanup_resources(&spill_dir).err();
            let staged_cleanup = staged_attachment_batch
                .take()
                .and_then(|batch| batch.rollback().err());
            let suffix = prepared_cleanup
                .map(|cleanup| format!("; prepared artifact cleanup failed: {cleanup}"))
                .into_iter()
                .chain(
                    staged_cleanup
                        .map(|cleanup| format!("; staged attachment cleanup failed: {cleanup}")),
                )
                .collect::<String>();
            return Err(IpcError::Validation(format!("{error}{suffix}")));
        }
    };
    if let Err(error) = emit_conversation_input_lifecycle_after(
        &app,
        state.app_state.storage.chat_events.as_ref(),
        &address,
        expected_queue_revision,
    ) {
        record_gui_transport_debt("steer_attempt_lifecycle", error);
    }
    let attempt = conversation_input_attempt(&started)?;
    let observer_service = service.clone();
    let observer_attempt = attempt.clone();
    let observer_address = address.clone();
    let observer_log = state.app_state.storage.chat_events.clone();
    let observer_app = app.clone();
    let observer_after_cursor = started.receipt.queue_revision;
    let observer_prepared = prepared.clone();
    let observer_spill_dir = spill_dir.clone();
    let terminal_service = service.clone();
    let terminal_address = address.clone();
    let terminal_attempt = attempt.clone();
    let terminal_app = app.clone();
    let terminal_log = state.app_state.storage.chat_events.clone();
    let terminal_projector: echo_agent_app_core::api::foreground_turn::ForegroundTerminalProjector =
        Arc::new(move |outcome| {
            let service = terminal_service.clone();
            let address = terminal_address.clone();
            let attempt = terminal_attempt.clone();
            let app = terminal_app.clone();
            let log = terminal_log.clone();
            Box::pin(async move {
                let after_cursor = settle_gui_input_attempt(&service, &attempt, &outcome).await?;
                if let Err(error) = emit_conversation_input_lifecycle_after(
                    &app,
                    log.as_ref(),
                    &address,
                    after_cursor,
                ) {
                    record_gui_transport_debt("live_terminal_input_lifecycle", error);
                }
                Ok(())
            })
        });
    let (steer_tx, steer_rx) = tokio::sync::oneshot::channel();
    let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
    let observer = async move {
        let result: Result<ConversationInputReceipt, String> = match steer_rx.await {
            Ok(steer_result) => match observer_service
                .observe_steer_through_drain(observer_attempt.clone(), steer_result)
                .await
            {
                Ok(receipt) if receipt.drained => {
                    if let Some(batch) = staged_attachment_batch.take() {
                        batch.commit();
                    }
                    Ok(receipt)
                }
                Ok(receipt) => {
                    let cleanup = observer_prepared
                        .cleanup_resources(&observer_spill_dir)
                        .err()
                        .map(|error| error.to_string())
                        .into_iter()
                        .chain(
                            staged_attachment_batch
                                .take()
                                .and_then(|batch| batch.rollback().err()),
                        )
                        .collect::<Vec<_>>();
                    if !cleanup.is_empty() {
                        record_gui_transport_debt(
                            "steer_input_resource_cleanup",
                            cleanup.join("; "),
                        );
                    }
                    Ok(receipt)
                }
                Err(error) => {
                    let reason = std::iter::once(error.to_string())
                        .chain(
                            observer_prepared
                                .cleanup_resources(&observer_spill_dir)
                                .err()
                                .map(|cleanup| cleanup.to_string()),
                        )
                        .chain(
                            staged_attachment_batch
                                .take()
                                .and_then(|batch| batch.rollback().err()),
                        )
                        .collect::<Vec<_>>()
                        .join("; ");
                    Err(reason)
                }
            },
            Err(error) => {
                let prepared_cleanup = observer_prepared
                    .cleanup_resources(&observer_spill_dir)
                    .err()
                    .map(|cleanup| cleanup.to_string());
                let staged_cleanup = staged_attachment_batch
                    .take()
                    .and_then(|batch| batch.rollback().err());
                let mut reasons = std::iter::once(format!("tracked steer handoff failed: {error}"))
                    .chain(prepared_cleanup)
                    .chain(staged_cleanup)
                    .collect::<Vec<_>>();
                let reason = reasons.join("; ");
                if let Err(recovery) = observer_service
                    .recovery_required_with_drain(observer_attempt.clone(), reason.clone(), false)
                    .await
                {
                    reasons.push(format!("recovery-required persistence failed: {recovery}"));
                }
                Err(reasons.join("; "))
            }
        };
        if let Err(error) = emit_conversation_input_lifecycle_after(
            &observer_app,
            observer_log.as_ref(),
            &observer_address,
            observer_after_cursor,
        ) {
            record_gui_transport_debt("steer_input_lifecycle", error);
        }
        let supervisor_result = result.as_ref().map(|_| ()).map_err(Clone::clone);
        let _ = observed_tx.send(result);
        supervisor_result
    };
    if let Err(error) = state
        .app_state
        .session
        .foreground_turns
        .supervise_input_lifecycle_scoped(
            &workspace_id,
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
            &conversation_id,
            &expected_turn_id,
            observer,
            terminal_projector,
        )
    {
        let cleanup = prepared
            .cleanup_resources(&spill_dir)
            .err()
            .map(|cleanup| cleanup.to_string());
        let reason = std::iter::once(error.to_string())
            .chain(cleanup)
            .collect::<Vec<_>>()
            .join("; ");
        let receipt = service
            .deferred(attempt, reason)
            .await
            .map_err(|error| IpcError::Internal(error.to_string()))?;
        return Ok(receipt);
    }
    let steer_result = agent
        .steer_input_tracked(Some(&expected_turn_id), steer_message)
        .await;
    if steer_result.is_ok()
        && let Err(error) = state
            .app_state
            .record_user_steer_for_active_turn(
                &workspace_id,
                &conversation_id,
                &expected_turn_id,
                &steer_text,
            )
            .await
    {
        tracing::debug!(%error, "GUI user steer was not bound to its TaskRun");
    }
    if steer_tx.send(steer_result).is_err() {
        let reason = "tracked steer observer ended before receipt handoff".to_string();
        let recovery = service
            .recovery_required_with_drain(attempt, reason.clone(), false)
            .await
            .err()
            .map(|error| format!("; recovery-required persistence failed: {error}"))
            .unwrap_or_default();
        return Err(IpcError::Internal(format!("{reason}{recovery}")));
    }
    observed_rx
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?
        .map_err(IpcError::Internal)
}

fn select_active_chat_turn(
    control: &echo_agent_app_core::api::foreground_turn::ForegroundTurnControl,
    workspace_id: &str,
    conversation_id: Option<&str>,
) -> Result<Option<echo_agent_app_core::api::foreground_turn::ForegroundTurnSnapshot>, IpcError> {
    use echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface;

    if let Some(conversation_id) = conversation_id {
        return Ok(control.snapshot_scoped(
            workspace_id,
            ForegroundTurnSurface::Gui,
            conversation_id,
        ));
    }
    let mut snapshots = control
        .snapshots(ForegroundTurnSurface::Gui)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    snapshots.retain(|snapshot| snapshot.workspace_id == workspace_id);
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
pub async fn get_active_chat_turn(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: Option<String>,
) -> Result<Option<echo_agent_app_core::api::foreground_turn::ForegroundTurnSnapshot>, IpcError> {
    select_active_chat_turn(
        &state.app_state.session.foreground_turns,
        &workspace_id,
        conversation_id.as_deref(),
    )
}

/// Replay the canonical ordered journal after the WebView's last applied cursor.
#[tauri::command]
pub async fn replay_chat_events(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: Option<String>,
    message_key: Option<String>,
    after_cursor: Option<u64>,
) -> Result<echo_agent_app_core::api::chat_event_log::ChatEventReplay, IpcError> {
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
        .replay(&workspace_id, conversation_id.as_deref(), &turn_id, 0)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let runtime = state
        .app_state
        .chat_runtime_for_scope(&workspace_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    ToolExecutionProjector::new(
        state.app_state.storage.tool_executions.clone(),
        runtime.task_runtime(),
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
            .replay(&workspace_id, conversation_id.as_deref(), &turn_id, cursor)
            .map_err(|error| IpcError::Internal(error.to_string()))
    }
}

async fn validate_queue_address(
    state: &TauriState,
    workspace_id: &str,
    conversation_id: &str,
) -> Result<(), IpcError> {
    let runtime = state
        .app_state
        .chat_runtime_for_scope(workspace_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let store = runtime
        .conversation_store()
        .ok_or_else(|| IpcError::Internal("Conversation store not available".to_string()))?;
    if store
        .get_conversation(conversation_id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?
        .is_none()
    {
        return Err(IpcError::NotFound(format!(
            "Conversation '{conversation_id}' not found in workspace '{workspace_id}'"
        )));
    }
    Ok(())
}

#[tauri::command]
pub async fn queue_chat_input(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    workspace_id: String,
    conversation_id: String,
    external_id: String,
    text: String,
    attachments: Option<Vec<echo_agent_app_core::api::types::AttachmentData>>,
) -> Result<ConversationInputReceipt, IpcError> {
    validate_queue_address(&state, &workspace_id, &conversation_id).await?;
    let address = conversation_input_address(&workspace_id, &conversation_id);
    let service = state.app_state.conversation_inputs();
    let before = service
        .list(&address)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let input_id = stable_scoped_input_id(&address, ConversationInputSource::Gui, &external_id)
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let receipt = service
        .submit(
            address.clone(),
            input_id,
            text,
            attachments.unwrap_or_default(),
        )
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    if let Err(error) = emit_conversation_input_lifecycle_after(
        &app,
        state.app_state.storage.chat_events.as_ref(),
        &address,
        before.queue_revision,
    ) {
        record_gui_transport_debt("queued_input_lifecycle", error);
    }
    Ok(receipt)
}

#[tauri::command]
pub async fn list_queued_chat_inputs(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: String,
) -> Result<echo_agent_app_core::api::conversation_input::ConversationInputFrontier, IpcError> {
    validate_queue_address(&state, &workspace_id, &conversation_id).await?;
    state
        .app_state
        .conversation_inputs()
        .list(&conversation_input_address(&workspace_id, &conversation_id))
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))
}

#[tauri::command]
pub async fn remove_queued_chat_input(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    identity: ConversationInputIdentity,
) -> Result<ConversationInputReceipt, IpcError> {
    validate_queue_address(
        &state,
        &identity.address.workspace_id,
        &identity.address.conversation_id,
    )
    .await?;
    let service = state.app_state.conversation_inputs();
    let before = service
        .list(&identity.address)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let receipt = service
        .cancel(identity.clone())
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    if let Err(error) = emit_conversation_input_lifecycle_after(
        &app,
        state.app_state.storage.chat_events.as_ref(),
        &identity.address,
        before.queue_revision,
    ) {
        record_gui_transport_debt("cancelled_input_lifecycle", error);
    }
    Ok(receipt)
}

#[tauri::command]
pub async fn reorder_queued_chat_inputs(
    state: tauri::State<'_, TauriState>,
    app: tauri::AppHandle,
    workspace_id: String,
    conversation_id: String,
    expected_queue_revision: u64,
    input_ids: Vec<String>,
) -> Result<u64, IpcError> {
    validate_queue_address(&state, &workspace_id, &conversation_id).await?;
    let address = conversation_input_address(&workspace_id, &conversation_id);
    let queue_revision = state
        .app_state
        .conversation_inputs()
        .reorder(&address, expected_queue_revision, input_ids)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    if let Err(error) = emit_conversation_input_lifecycle_after(
        &app,
        state.app_state.storage.chat_events.as_ref(),
        &address,
        expected_queue_revision,
    ) {
        record_gui_transport_debt("reordered_input_lifecycle", error);
    }
    Ok(queue_revision)
}

/// Cancel an active chat stream.
fn request_chat_cancel(
    control: &echo_agent_app_core::api::foreground_turn::ForegroundTurnControl,
    workspace_id: &str,
    conversation_id: &str,
    root_turn_id: &str,
) -> Result<
    Option<echo_agent_app_core::api::foreground_turn::ForegroundTurnSettlementWaiter>,
    IpcError,
> {
    use echo_agent_app_core::api::foreground_turn::{ForegroundTurnError, ForegroundTurnSurface};

    match control.request_root_cancel_scoped(
        workspace_id,
        ForegroundTurnSurface::Gui,
        conversation_id,
        root_turn_id,
    ) {
        Ok(waiter) => Ok(Some(waiter)),
        Err(ForegroundTurnError::NoActiveTurn { .. }) => Ok(None),
        Err(error) => Err(IpcError::Validation(error.to_string())),
    }
}

async fn append_chat_projection(
    state: &TauriState,
    workspace_id: &str,
    conversation_id: &str,
    root_turn_id: &str,
    event: ChatDriverEvent,
) -> Result<(), IpcError> {
    let runtime = state
        .app_state
        .chat_runtime_for_scope(workspace_id)
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let workspace_receipt = runtime.workspace_io_receipt();
    let chat_events = state.app_state.storage.chat_events.clone();
    let workspace_id = workspace_id.to_string();
    let conversation_id = conversation_id.to_string();
    let root_turn_id = root_turn_id.to_string();
    state
        .app_state
        .session
        .product_data_io
        .run("append GUI chat projection", move || {
            let _workspace_receipt = workspace_receipt;
            chat_events.append(&workspace_id, Some(&conversation_id), &root_turn_id, event)
        })
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?
        .map(|_| ())
        .map_err(|error| IpcError::Internal(error.to_string()))
}

#[tauri::command]
pub async fn cancel_chat(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: String,
    expected_root_turn_id: String,
    expected_active_turn_id: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    if let Some(expected_active_turn_id) = expected_active_turn_id
        && let Some(snapshot) = state.app_state.session.foreground_turns.snapshot_scoped(
            &workspace_id,
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
            &conversation_id,
        )
        && snapshot.active_turn_id != expected_active_turn_id
    {
        return Err(IpcError::Validation(format!(
            "active chat turn mismatch: expected {expected_active_turn_id}, actual {}",
            snapshot.active_turn_id
        )));
    }
    let Some(waiter) = request_chat_cancel(
        &state.app_state.session.foreground_turns,
        &workspace_id,
        &conversation_id,
        &expected_root_turn_id,
    )?
    else {
        return Ok(serde_json::json!({
            "success": true,
            "turn_id": expected_root_turn_id,
            "status": "already_settled",
        }));
    };

    // Reject pending HITL before waiting so parked execution can reach its
    // terminal outcome. Ownership remains registered until that settlement.
    cancel_pending_hitl(Some(&expected_root_turn_id), "cancelled by user").await;
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
fn validate_hitl_response_scope(
    state: &TauriState,
    workspace_id: &str,
    conversation_id: &str,
    expected_root_turn_id: Option<&str>,
    expected_active_turn_id: Option<&str>,
) -> Result<(), IpcError> {
    let root_turn_id = expected_root_turn_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| IpcError::Validation("expected_root_turn_id is required".to_string()))?;
    let active_turn_id = expected_active_turn_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| IpcError::Validation("expected_active_turn_id is required".to_string()))?;
    let snapshots = state
        .app_state
        .session
        .foreground_turns
        .snapshots_for_conversation_scoped(workspace_id, conversation_id)
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    if snapshots.iter().any(|snapshot| {
        snapshot.root_turn_id == root_turn_id && snapshot.active_turn_id == active_turn_id
    }) {
        Ok(())
    } else {
        Err(IpcError::Validation(
            "HITL response does not match an active workspace turn".to_string(),
        ))
    }
}

async fn settle_orphaned_hitl_projection(
    state: &TauriState,
    workspace_id: &str,
    conversation_id: &str,
    root_turn_id: Option<&str>,
) -> Result<(), IpcError> {
    let Some(root_turn_id) = root_turn_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    if let Ok(snapshots) = state
        .app_state
        .session
        .foreground_turns
        .snapshots_for_conversation_scoped(workspace_id, conversation_id)
    {
        for snapshot in snapshots
            .into_iter()
            .filter(|snapshot| snapshot.root_turn_id == root_turn_id)
        {
            let _ = state
                .app_state
                .session
                .foreground_turns
                .request_root_cancel_scoped(
                    workspace_id,
                    snapshot.surface,
                    conversation_id,
                    root_turn_id,
                );
        }
    }
    append_chat_projection(
        state,
        workspace_id,
        conversation_id,
        root_turn_id,
        ChatDriverEvent::TurnStatus {
            status: "failed".to_string(),
        },
    )
    .await
}

// Tauri exposes these fields as individual IPC arguments; grouping them would
// change the frontend wire contract rather than simplify internal logic.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn send_approval_response(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: String,
    expected_root_turn_id: Option<String>,
    expected_active_turn_id: Option<String>,
    request_id: String,
    approved: bool,
    reason: Option<String>,
    scope: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    if let Err(error) = validate_hitl_response_scope(
        &state,
        &workspace_id,
        &conversation_id,
        expected_root_turn_id.as_deref(),
        expected_active_turn_id.as_deref(),
    ) {
        settle_orphaned_hitl_projection(
            &state,
            &workspace_id,
            &conversation_id,
            expected_root_turn_id.as_deref(),
        )
        .await?;
        return Err(error);
    }
    let req = lock_std(&PENDING_RESPONSES, "pending GUI HITL responses").remove(&request_id);
    if let Some(req) = req {
        let _ = req.tx.send(PendingResponse::Approval {
            approved,
            reason,
            scope,
        });
        Ok(serde_json::json!({"success": true}))
    } else {
        settle_orphaned_hitl_projection(
            &state,
            &workspace_id,
            &conversation_id,
            expected_root_turn_id.as_deref(),
        )
        .await?;
        Err(IpcError::NotFound(format!(
            "Approval request '{}' not found or expired",
            request_id
        )))
    }
}

/// Respond to an input request.
#[tauri::command]
pub async fn send_input_response(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: String,
    expected_root_turn_id: Option<String>,
    expected_active_turn_id: Option<String>,
    request_id: String,
    text: String,
) -> Result<serde_json::Value, IpcError> {
    if let Err(error) = validate_hitl_response_scope(
        &state,
        &workspace_id,
        &conversation_id,
        expected_root_turn_id.as_deref(),
        expected_active_turn_id.as_deref(),
    ) {
        settle_orphaned_hitl_projection(
            &state,
            &workspace_id,
            &conversation_id,
            expected_root_turn_id.as_deref(),
        )
        .await?;
        return Err(error);
    }
    let req = lock_std(&PENDING_RESPONSES, "pending GUI HITL responses").remove(&request_id);
    if let Some(req) = req {
        let _ = req.tx.send(PendingResponse::Input { text });
        Ok(serde_json::json!({"success": true}))
    } else {
        settle_orphaned_hitl_projection(
            &state,
            &workspace_id,
            &conversation_id,
            expected_root_turn_id.as_deref(),
        )
        .await?;
        Err(IpcError::NotFound(format!(
            "Input request '{}' not found or expired",
            request_id
        )))
    }
}

/// Respond to a selection request.
// Keep parity with the selection-response IPC schema consumed by all GUIs.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn send_selection_response(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: String,
    expected_root_turn_id: Option<String>,
    expected_active_turn_id: Option<String>,
    request_id: String,
    selection: String,
    instructions: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    if let Err(error) = validate_hitl_response_scope(
        &state,
        &workspace_id,
        &conversation_id,
        expected_root_turn_id.as_deref(),
        expected_active_turn_id.as_deref(),
    ) {
        settle_orphaned_hitl_projection(
            &state,
            &workspace_id,
            &conversation_id,
            expected_root_turn_id.as_deref(),
        )
        .await?;
        return Err(error);
    }
    let req = lock_std(&PENDING_RESPONSES, "pending GUI HITL responses").remove(&request_id);
    if let Some(req) = req {
        let _ = req.tx.send(PendingResponse::Selection {
            selection,
            instructions,
        });
        Ok(serde_json::json!({"success": true}))
    } else {
        settle_orphaned_hitl_projection(
            &state,
            &workspace_id,
            &conversation_id,
            expected_root_turn_id.as_deref(),
        )
        .await?;
        Err(IpcError::NotFound(format!(
            "Selection request '{}' not found or expired",
            request_id
        )))
    }
}

/// Commits app-owned TaskRuntime events to the canonical chat journal before
/// publishing them, then derives the same tool detail used by every surface.
pub(crate) struct TauriExecutionProjector {
    app: Option<tauri::AppHandle>,
    projector: Arc<JournaledExecutionProjector>,
}

impl TauriExecutionProjector {
    pub(crate) fn new(
        app: tauri::AppHandle,
        tool_executions: Arc<ToolExecutionRepository>,
        chat_events: Arc<ChatEventLog>,
        task_runtime_store: Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
    ) -> Self {
        Self {
            app: Some(app),
            projector: Arc::new(JournaledExecutionProjector::new(
                chat_events,
                tool_executions,
                task_runtime_store,
            )),
        }
    }

    pub(crate) fn emit(&self, event: ExecEvent) {
        match self.projector.project(event) {
            Ok(projected) => {
                self.emit_updates(&projected.tool_updates);
                if let Some(app) = self.app.as_ref() {
                    let _ = emit_chat_envelope(app, &projected.envelope);
                }
            }
            Err(error) => {
                tracing::error!(%error, "failed to commit TaskRuntime execution event");
            }
        }
    }

    #[cfg(test)]
    fn without_app(
        chat_events: Arc<ChatEventLog>,
        tool_executions: Arc<ToolExecutionRepository>,
        task_runtime_store: Arc<echo_agent_app_core::api::tasks::task_runtime::TaskRuntimeStore>,
    ) -> Self {
        Self {
            app: None,
            projector: Arc::new(JournaledExecutionProjector::new(
                chat_events,
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
    emit_envelope: Arc<dyn Fn(&ChatEventEnvelope) -> bool + Send + Sync>,
    emit_tool_projection: Arc<dyn Fn(&ToolExecutionProjectionUpdate) -> bool + Send + Sync>,
}

pub(crate) fn tauri_chat_sink(
    app: tauri::AppHandle,
    workspace_id: String,
    message_key: String,
    conversation_id: Option<String>,
    tool_executions: Arc<echo_agent_app_core::api::tool_execution::ToolExecutionRepository>,
    chat_events: Arc<ChatEventLog>,
) -> Arc<dyn ChatSink> {
    let envelope_app = app.clone();
    let projection_app = app.clone();
    let renderer = Arc::new(TauriChatSink {
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
        workspace_id,
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

impl echo_agent_app_core::api::chat_driver::ChatSink for TauriChatSink {
    fn on_event(&self, _event: ChatDriverEvent) -> bool {
        tracing::error!("TauriChatSink received an event before the ordered chat journal boundary");
        false
    }

    fn on_journaled_event(&self, envelope: ChatEventEnvelope) -> bool {
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
    use echo_agent_app_core::api::chat_event_log::ChatEventRetention;
    use echo_agent_app_core::api::tool_execution::ToolExecutionStatus;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RejectingChatSink;

    impl ChatSink for RejectingChatSink {
        fn on_event(&self, _event: ChatDriverEvent) -> bool {
            false
        }
    }

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
    fn send_chat_request_uses_one_camel_case_wire_object() -> Result<(), Box<dyn std::error::Error>>
    {
        let request: SendChatMessageRequest = serde_json::from_value(serde_json::json!({
            "workspaceId": "workspace-1",
            "message": "hello",
            "conversationId": "conversation-1",
            "messageKey": "message-1",
            "attachments": null,
            "inputIdentity": null,
            "expectedQueueRevision": null
        }))?;
        assert_eq!(request.workspace_id, "workspace-1");
        assert_eq!(request.conversation_id.as_deref(), Some("conversation-1"));
        assert_eq!(request.message_key.as_deref(), Some("message-1"));
        Ok(())
    }

    #[test]
    fn terminal_transport_rejection_is_reported_without_panicking() {
        let sink: Arc<dyn ChatSink> = Arc::new(RejectingChatSink);
        assert!(!emit_gui_turn_status(&sink, "completed"));
    }

    #[tokio::test]
    async fn pre_driver_terminal_settlement_remains_replayable_when_not_drained()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TestDir::new()?;
        let log = Arc::new(ChatEventLog::open(
            temp.path().join("pre-driver-ingress"),
            ChatEventRetention::default(),
        )?);
        let service =
            echo_agent_app_core::api::conversation_input::ConversationInputService::new(log);
        let address = conversation_input_address("workspace-1", "conversation-1");
        let persisted = service
            .submit(
                address.clone(),
                "input-pre-driver".to_string(),
                "retry me".to_string(),
                Vec::new(),
            )
            .await?;
        let frontier = service.list(&address).await?;
        let started = service
            .dispatch_selected(
                persisted.identity,
                frontier.queue_revision,
                "turn-pre-driver".to_string(),
            )
            .await?;
        let attempt = conversation_input_attempt(&started)?;

        settle_gui_input_attempt(
            &service,
            &attempt,
            &echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(
                echo_agent::error::AgentFailure::message(
                    "pre_driver",
                    "driver preparation failed before observer invocation",
                ),
            ),
        )
        .await?;

        let settled = service.list(&address).await?;
        let receipt = settled
            .items
            .first()
            .map(|projection| &projection.receipt)
            .ok_or_else(|| std::io::Error::other("replayable input left the frontier"))?;
        assert_eq!(receipt.phase, ConversationInputPhase::TurnSettled);
        assert!(!receipt.drained);
        assert!(receipt.is_dispatchable());
        Ok(())
    }

    #[tokio::test]
    async fn exact_projector_survives_stream_eviction_and_keeps_observed_drain_non_replayable()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TestDir::new()?;
        let root = temp.path().join("exact-projector-eviction");
        let log = Arc::new(ChatEventLog::open(&root, ChatEventRetention::default())?);
        let service = echo_agent_app_core::api::conversation_input::ConversationInputService::new(
            Arc::clone(&log),
        );
        let address = conversation_input_address("workspace-1", "conversation-held");
        let persisted = service
            .submit(
                address.clone(),
                "input-held".to_string(),
                "execute exactly once".to_string(),
                Vec::new(),
            )
            .await?;
        let frontier = service.list(&address).await?;
        let started = service
            .dispatch_selected(
                persisted.identity,
                frontier.queue_revision,
                "turn-held".to_string(),
            )
            .await?;
        let attempt = conversation_input_attempt(&started)?;
        attempt.observation.mark_drained();
        service.mailbox_accepted(attempt.clone()).await?;
        service.drained(attempt.clone()).await?;

        for index in 0..132_u32 {
            let noise_address =
                conversation_input_address("workspace-1", &format!("conversation-noise-{index}"));
            service
                .submit(
                    noise_address,
                    format!("input-noise-{index}"),
                    format!("noise {index}"),
                    Vec::new(),
                )
                .await?;
        }

        settle_gui_input_attempt(
            &service,
            &attempt,
            &echo_agent_app_core::api::chat_driver::TurnOutcome::Completed,
        )
        .await?;
        assert!(service.list(&address).await?.items.is_empty());
        drop(service);
        drop(log);

        let reopened = echo_agent_app_core::api::conversation_input::ConversationInputService::new(
            Arc::new(ChatEventLog::open(&root, ChatEventRetention::default())?),
        );
        assert!(reopened.list(&address).await?.items.is_empty());
        let duplicate = reopened
            .submit(
                address,
                "input-held".to_string(),
                "execute exactly once".to_string(),
                Vec::new(),
            )
            .await?;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.phase, ConversationInputPhase::TurnSettled);
        assert!(duplicate.drained);
        assert!(duplicate.blocks_replay());
        Ok(())
    }

    #[tokio::test]
    async fn live_terminal_projector_commits_before_foreground_waiter_release()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TestDir::new()?;
        let log = Arc::new(ChatEventLog::open(
            temp.path().join("live-terminal-projector"),
            ChatEventRetention::default(),
        )?);
        let service =
            echo_agent_app_core::api::conversation_input::ConversationInputService::new(log);
        let address = conversation_input_address("workspace-1", "conversation-live");
        let persisted = service
            .submit(
                address.clone(),
                "input-live".to_string(),
                "live guidance".to_string(),
                Vec::new(),
            )
            .await?;
        let frontier = service.list(&address).await?;
        let started = service
            .dispatch_selected(
                persisted.identity,
                frontier.queue_revision,
                "turn-live".to_string(),
            )
            .await?;
        let attempt = conversation_input_attempt(&started)?;
        let control = echo_agent_app_core::api::foreground_turn::ForegroundTurnControl::default();
        let lease = control.begin_scoped(
            "workspace-1",
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
            "conversation-live",
            "turn-live",
        )?;
        let waiter = control.settlement_waiter_scoped(
            "workspace-1",
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
            "conversation-live",
            "turn-live",
        )?;
        let projector_service = service.clone();
        let projector_attempt = attempt;
        let projector_entered = Arc::new(tokio::sync::Notify::new());
        let projector_release = Arc::new(tokio::sync::Notify::new());
        let entered_for_projector = Arc::clone(&projector_entered);
        let release_for_projector = Arc::clone(&projector_release);
        let projector: echo_agent_app_core::api::foreground_turn::ForegroundTerminalProjector =
            Arc::new(move |outcome| {
                let service = projector_service.clone();
                let attempt = projector_attempt.clone();
                let entered = Arc::clone(&entered_for_projector);
                let release = Arc::clone(&release_for_projector);
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    settle_gui_input_attempt(&service, &attempt, &outcome)
                        .await
                        .map(|_| ())
                })
            });
        control.supervise_input_lifecycle_scoped(
            "workspace-1",
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
            "conversation-live",
            "turn-live",
            async { Ok(()) },
            projector,
        )?;
        let settling = tokio::spawn(async move {
            lease
                .settle_after_observers(
                    echo_agent_app_core::api::chat_driver::TurnOutcome::Completed,
                )
                .await
        });
        projector_entered.notified().await;
        let mut waiting = Box::pin(waiter.wait());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        projector_release.notify_one();
        settling.await??;
        let projected = service.list(&address).await?;
        let receipt = projected
            .items
            .first()
            .map(|item| &item.receipt)
            .ok_or_else(|| std::io::Error::other("live terminal projection is missing"))?;
        assert_eq!(receipt.phase, ConversationInputPhase::TurnSettled);
        let settlement = waiting.await?;
        assert_eq!(
            settlement.outcome,
            echo_agent_app_core::api::chat_driver::TurnOutcome::Completed
        );
        Ok(())
    }

    #[tokio::test]
    async fn live_observer_failure_returns_and_shutdown_is_bounded()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TestDir::new()?;
        let log = Arc::new(ChatEventLog::open(
            temp.path().join("live-observer-failure"),
            ChatEventRetention::default(),
        )?);
        let service =
            echo_agent_app_core::api::conversation_input::ConversationInputService::new(log);
        let address = conversation_input_address("workspace-1", "conversation-failure");
        let persisted = service
            .submit(
                address.clone(),
                "input-live-failure".to_string(),
                "ambiguous guidance".to_string(),
                Vec::new(),
            )
            .await?;
        let frontier = service.list(&address).await?;
        let started = service
            .dispatch_selected(
                persisted.identity,
                frontier.queue_revision,
                "turn-live-failure".to_string(),
            )
            .await?;
        let attempt = conversation_input_attempt(&started)?;
        let control = echo_agent_app_core::api::foreground_turn::ForegroundTurnControl::default();
        let lease = control.begin_scoped(
            "workspace-1",
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
            "conversation-failure",
            "turn-live-failure",
        )?;
        let waiter = control.settlement_waiter_scoped(
            "workspace-1",
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
            "conversation-failure",
            "turn-live-failure",
        )?;
        let observer_service = service.clone();
        let observer_attempt = attempt.clone();
        let projector_service = service.clone();
        let projector_attempt = attempt;
        let projector: echo_agent_app_core::api::foreground_turn::ForegroundTerminalProjector =
            Arc::new(move |outcome| {
                let service = projector_service.clone();
                let attempt = projector_attempt.clone();
                Box::pin(async move {
                    settle_gui_input_attempt(&service, &attempt, &outcome)
                        .await
                        .map(|_| ())
                })
            });
        control.supervise_input_lifecycle_scoped(
            "workspace-1",
            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Gui,
            "conversation-failure",
            "turn-live-failure",
            async move {
                let reason = "injected permanent live observer failure".to_string();
                observer_service
                    .recovery_required_with_drain(observer_attempt, reason.clone(), false)
                    .await
                    .map_err(|error| error.to_string())?;
                Err(reason)
            },
            projector,
        )?;

        let settlement = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            lease.settle_after_observers(
                echo_agent_app_core::api::chat_driver::TurnOutcome::Completed,
            ),
        )
        .await
        .map_err(|_| std::io::Error::other("GUI observer settlement exceeded its bound"))??;
        assert!(matches!(
            settlement.outcome,
            echo_agent_app_core::api::chat_driver::TurnOutcome::Failed(_)
        ));
        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), waiter.wait())
            .await
            .map_err(|_| std::io::Error::other("GUI settlement waiter exceeded its bound"))??;
        assert_eq!(observed, settlement);
        assert!(service.list(&address).await?.items.is_empty());
        tokio::time::timeout(std::time::Duration::from_secs(1), control.shutdown())
            .await
            .map_err(|_| std::io::Error::other("GUI shutdown waited forever on observer debt"))??;
        assert!(!control.has_active_turns());
        Ok(())
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
            "workspace-1",
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
        let replay = log.replay("workspace-1", Some("conversation-wire"), "root-message", 0)?;
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
            "workspace-1",
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
            .summaries_for_conversation("workspace-1", "conversation-1")
            .into_iter()
            .find(|summary| summary.call_id == "call-1")
            .ok_or_else(|| std::io::Error::other("tool result projection missing"))?
            .detail_ref;
        drop(sink);
        drop(repository);

        let rebound = ToolExecutionRepository::open(tool_root)?;
        let summary = rebound
            .summaries_for_conversation("workspace-1", "conversation-1")
            .into_iter()
            .find(|summary| summary.call_id == "call-1")
            .ok_or_else(|| std::io::Error::other("tool result did not survive reopen"))?;
        assert_eq!(summary.status, ToolExecutionStatus::Succeeded);
        let detail = rebound.detail_manifest("workspace-1", &detail_ref)?;
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
                "workspace-1",
                Some("conversation-replay"),
                "root-replay",
                ChatDriverEvent::Agent(Box::new(envelope)),
            )?;
        }

        let repository = Arc::new(ToolExecutionRepository::open(temp.path().join("tools"))?);
        let replay = log.replay("workspace-1", Some("conversation-replay"), "root-replay", 0)?;
        let projector = ToolExecutionProjector::new(repository.clone(), None);
        projector.rebuild_from_retained(&replay.events)?;
        projector.rebuild_from_retained(&replay.events)?;

        let summaries = repository.summaries_for_conversation("workspace-1", "conversation-replay");
        assert_eq!(summaries.len(), 1);
        let summary = summaries
            .first()
            .ok_or_else(|| std::io::Error::other("replayed tool detail missing"))?;
        assert_eq!(summary.status, ToolExecutionStatus::Succeeded);
        let detail = repository.detail_manifest("workspace-1", &summary.detail_ref)?;
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
            "workspace-1",
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

        let replay = log.replay("workspace-1", Some("conversation-hitl"), "root-hitl", 0)?;
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
    use echo_agent_app_core::api::tasks::task_runtime::types::RuntimeEventKind;
    use echo_agent_app_core::api::tasks::task_runtime::{
        AttendedMode, DomainProfile, TaskRuntimeStore,
    };
    use echo_agent_app_core::api::tool_execution::ToolExecutionStatus;
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
        let repository = Arc::new(ToolExecutionRepository::open(temp.path().join("tools"))?);
        let chat_events = Arc::new(ChatEventLog::open(
            temp.path().join("chat-events"),
            echo_agent_app_core::api::chat_event_log::ChatEventRetention::default(),
        )?);
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
        let projector = TauriExecutionProjector::without_app(
            chat_events.clone(),
            repository.clone(),
            runtime.clone(),
        );
        let execution_id = "run-1:task-1:1:1";

        projector.emit(
            ExecEvent::subagent(
                "workspace-1",
                "conversation-1",
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
            "workspace-1",
            "conversation-1",
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
            "workspace-1",
            "conversation-1",
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

        let summaries = repository.summaries_for_conversation("workspace-1", "conversation-1");
        let completed = summaries
            .iter()
            .find(|summary| summary.call_id == "call-1")
            .ok_or_else(|| "missing completed tool summary".to_string())?;
        assert_eq!(completed.status, ToolExecutionStatus::Succeeded);
        assert_eq!(completed.run_id.as_deref(), Some("run-1"));
        let detail = repository.detail_manifest("workspace-1", &completed.detail_ref)?;
        assert_eq!(
            detail
                .result
                .as_ref()
                .and_then(|result| result.metadata.get("source"))
                .map(String::as_str),
            Some("canonical-result")
        );
        let output = repository.read_output("workspace-1", &completed.detail_ref, None, 1024)?;
        assert_eq!(
            output.chunks.first().map(|chunk| chunk.text.as_str()),
            Some("main output")
        );

        projector.emit(ExecEvent::subagent(
            "workspace-1",
            "conversation-1",
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
            "workspace-1",
            "conversation-1",
            "run-1",
            "task-1",
            execution_id,
            RuntimeEventKind::Completed,
            serde_json::json!({}),
        ));

        let summaries = repository.summaries_for_conversation("workspace-1", "conversation-1");
        let orphaned = summaries
            .iter()
            .find(|summary| summary.call_id == "call-2")
            .ok_or_else(|| "missing orphaned tool summary".to_string())?;
        assert_eq!(orphaned.status, ToolExecutionStatus::Unknown);
        assert_eq!(
            chat_events
                .replay("workspace-1", Some("conversation-1"), "message-1", 0)?
                .events
                .len(),
            5
        );
        Ok(())
    }
}

#[cfg(test)]
mod foreground_turn_command_tests {
    use super::*;
    use echo_agent_app_core::api::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

    #[tokio::test]
    async fn active_snapshot_returns_real_message_scope_without_product_conversation()
    -> Result<(), Box<dyn std::error::Error>> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin(ForegroundTurnSurface::Gui, "message:turn-1", "turn-1")?;
        let snapshot = select_active_chat_turn(&control, "global", None)?
            .ok_or_else(|| "missing active snapshot".to_string())?;
        assert_eq!(snapshot.conversation_id, "message:turn-1");
        assert_eq!(snapshot.root_turn_id, "turn-1");
        assert_eq!(snapshot.active_turn_id, "turn-1");
        lease
            .settle_after_observers(echo_agent_app_core::api::chat_driver::TurnOutcome::Completed)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn active_snapshot_requires_scope_when_multiple_gui_turns_exist()
    -> Result<(), Box<dyn std::error::Error>> {
        let control = ForegroundTurnControl::default();
        let first = control.begin(ForegroundTurnSurface::Gui, "conversation-1", "turn-1")?;
        let second = control.begin(ForegroundTurnSurface::Gui, "conversation-2", "turn-2")?;
        assert!(matches!(
            select_active_chat_turn(&control, "global", None),
            Err(IpcError::Validation(message))
                if message == "active_chat_turn_ambiguous:conversation_id_required"
        ));
        let snapshot = select_active_chat_turn(&control, "global", Some("conversation-2"))?
            .ok_or_else(|| "missing exact active snapshot".to_string())?;
        assert_eq!(snapshot.root_turn_id, "turn-2");
        assert_eq!(snapshot.active_turn_id, "turn-2");
        first
            .settle_after_observers(echo_agent_app_core::api::chat_driver::TurnOutcome::Completed)
            .await?;
        second
            .settle_after_observers(echo_agent_app_core::api::chat_driver::TurnOutcome::Completed)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn stop_is_idempotent_after_the_exact_gui_turn_settles()
    -> Result<(), Box<dyn std::error::Error>> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin_scoped(
            "workspace-1",
            ForegroundTurnSurface::Gui,
            "conversation-1",
            "turn-1",
        )?;
        lease
            .settle_after_observers(echo_agent_app_core::api::chat_driver::TurnOutcome::Completed)
            .await?;

        let waiter = request_chat_cancel(&control, "workspace-1", "conversation-1", "turn-1")?;

        assert!(waiter.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn stop_still_rejects_a_stale_root_when_another_gui_turn_is_active()
    -> Result<(), Box<dyn std::error::Error>> {
        let control = ForegroundTurnControl::default();
        let lease = control.begin_scoped(
            "workspace-1",
            ForegroundTurnSurface::Gui,
            "conversation-1",
            "current-turn",
        )?;

        assert!(matches!(
            request_chat_cancel(
                &control,
                "workspace-1",
                "conversation-1",
                "stale-turn",
            ),
            Err(IpcError::Validation(message)) if message.contains("foreground turn mismatch")
        ));
        assert!(!lease.cancellation_token().is_cancelled());
        lease
            .settle_after_observers(echo_agent_app_core::api::chat_driver::TurnOutcome::Completed)
            .await?;
        Ok(())
    }
}
