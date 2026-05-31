//! Human checkpoint gate -- pauses pipelines for user input.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

/// Request for human input at a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanCheckpointRequest {
    /// What the system is asking the human about
    pub prompt: String,
    /// Context data (e.g., the draft outline for review)
    pub context: serde_json::Value,
    /// Options for the human (e.g., ["approve", "revise", "cancel"])
    pub options: Vec<String>,
    /// Phase that is waiting for input
    pub waiting_phase: String,
}

/// Response from the human.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanCheckpointResponse {
    /// Selected option
    pub selection: String,
    /// Optional additional instructions
    pub instructions: Option<String>,
}

struct PendingRequest {
    request: HumanCheckpointRequest,
    sender: oneshot::Sender<HumanCheckpointResponse>,
}

/// Gate that blocks a pipeline until human input is received.
pub struct HumanCheckpointGate {
    pending: Arc<Mutex<HashMap<String, PendingRequest>>>,
}

impl HumanCheckpointGate {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a pending human checkpoint request and wait for response.
    /// This blocks until the human responds or the cancel token fires.
    pub async fn request_input(
        &self,
        task_id: &str,
        request: HumanCheckpointRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<HumanCheckpointResponse> {
        let (sender, receiver) = oneshot::channel();

        {
            let mut pending = self.pending.lock().await;
            pending.insert(
                task_id.to_string(),
                PendingRequest { request, sender },
            );
        }

        tracing::info!("Human checkpoint: waiting for input on task {}", task_id);

        // Wait for response or cancellation
        tokio::select! {
            response = receiver => {
                response.map_err(|_| anyhow::anyhow!("Human checkpoint response channel dropped"))
            }
            _ = cancel.cancelled() => {
                // Clean up on cancellation
                let mut pending = self.pending.lock().await;
                pending.remove(task_id);
                Err(anyhow::anyhow!("Task cancelled while waiting for human input"))
            }
        }
    }

    /// Called by CLI/Web when the human responds.
    pub async fn respond(&self, task_id: &str, response: HumanCheckpointResponse) -> bool {
        let mut pending = self.pending.lock().await;
        if let Some(req) = pending.remove(task_id) {
            req.sender.send(response).is_ok()
        } else {
            false
        }
    }

    /// List pending human checkpoint requests.
    pub async fn pending_requests(&self) -> Vec<(String, HumanCheckpointRequest)> {
        let pending = self.pending.lock().await;
        pending
            .iter()
            .map(|(id, req)| (id.clone(), req.request.clone()))
            .collect()
    }
}

impl Default for HumanCheckpointGate {
    fn default() -> Self {
        Self::new()
    }
}
