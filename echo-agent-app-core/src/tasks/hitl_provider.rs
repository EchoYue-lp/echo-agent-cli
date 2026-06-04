//! Background task human-in-the-loop provider.
//!
//! Routes HITL requests from background task agents through the TaskEventBus
//! so frontends (Tauri, CLI) can display approval/input requests and send responses.

use dashmap::DashMap;
use echo_agent::error::Result;
use echo_agent::human_loop::{
    HumanLoopKind, HumanLoopProvider, HumanLoopRequest, HumanLoopResponse,
};
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot};

/// Pending HITL request waiting for a response.
struct PendingRequest {
    sender: oneshot::Sender<HumanLoopResponse>,
    request: HumanLoopRequest,
}

/// Event emitted when a background task needs human input.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HitlEvent {
    pub task_id: String,
    pub request_id: String,
    pub kind: String,
    pub prompt: String,
    pub tool_name: Option<String>,
    pub args: Option<serde_json::Value>,
    pub risk_level: Option<String>,
    /// Selection-specific fields
    pub options: Option<Vec<String>>,
    pub context: Option<serde_json::Value>,
    pub phase: Option<String>,
}

/// HumanLoopProvider for background tasks.
///
/// When an agent running in the background needs human approval or input,
/// this provider emits a `HitlEvent` via broadcast and blocks until the
/// frontend responds via `respond()`.
pub struct BackgroundTaskHumanProvider {
    /// Map of request_id -> pending request
    pending: Arc<DashMap<String, PendingRequest>>,
    /// Event bus to emit HITL request events
    pub event_tx: Arc<broadcast::Sender<HitlEvent>>,
}

impl BackgroundTaskHumanProvider {
    pub fn new() -> (Self, broadcast::Receiver<HitlEvent>) {
        let (tx, rx) = broadcast::channel(100);
        (
            Self {
                pending: Arc::new(DashMap::new()),
                event_tx: Arc::new(tx),
            },
            rx,
        )
    }

    /// Send a response to a pending HITL request.
    pub fn respond(&self, request_id: &str, response: HumanLoopResponse) -> bool {
        if let Some((_, pending)) = self.pending.remove(request_id) {
            let _ = pending.sender.send(response);
            true
        } else {
            false
        }
    }

    /// Check if there are pending HITL requests.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Get list of pending request IDs.
    pub fn pending_request_ids(&self) -> Vec<String> {
        self.pending.iter().map(|e| e.key().clone()).collect()
    }

    /// Get details of a pending request.
    pub fn get_pending(&self, request_id: &str) -> Option<HitlEvent> {
        self.pending.get(request_id).map(|entry| {
            let req = &entry.request;
            HitlEvent {
                task_id: String::new(), // will be filled by caller
                request_id: request_id.to_string(),
                kind: match req.kind {
                    HumanLoopKind::Approval => "approval".to_string(),
                    HumanLoopKind::Input => "input".to_string(),
                    HumanLoopKind::Selection => "selection".to_string(),
                },
                prompt: req.prompt.clone(),
                tool_name: req.tool_name.clone(),
                args: req.args.clone(),
                risk_level: req.risk_level.as_ref().map(|r| format!("{:?}", r)),
                options: req.options.clone(),
                context: req.context.clone(),
                phase: req.phase.clone(),
            }
        })
    }
}

impl HumanLoopProvider for BackgroundTaskHumanProvider {
    fn request(&self, req: HumanLoopRequest) -> BoxFuture<'_, Result<HumanLoopResponse>> {
        let (tx, rx) = oneshot::channel();
        let request_id = uuid::Uuid::new_v4().to_string();

        // Build the event
        let event = HitlEvent {
            task_id: String::new(), // caller should fill
            request_id: request_id.clone(),
            kind: match req.kind {
                HumanLoopKind::Approval => "approval".to_string(),
                HumanLoopKind::Input => "input".to_string(),
                HumanLoopKind::Selection => "selection".to_string(),
            },
            prompt: req.prompt.clone(),
            tool_name: req.tool_name.clone(),
            args: req.args.clone(),
            risk_level: req.risk_level.as_ref().map(|r| format!("{:?}", r)),
            options: req.options.clone(),
            context: req.context.clone(),
            phase: req.phase.clone(),
        };

        // Store pending request
        self.pending.insert(
            request_id.clone(),
            PendingRequest {
                sender: tx,
                request: req,
            },
        );

        // Emit event
        let _ = self.event_tx.send(event);

        tracing::info!(request_id = %request_id, "Background task HITL request emitted");

        Box::pin(async move {
            match rx.await {
                Ok(response) => Ok(response),
                Err(_) => {
                    tracing::warn!("HITL response channel closed, defaulting to Rejected");
                    Ok(HumanLoopResponse::Rejected {
                        reason: Some("Response channel closed".to_string()),
                    })
                }
            }
        })
    }
}
