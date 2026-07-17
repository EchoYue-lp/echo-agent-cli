//! TUI HITL Provider — non-blocking human-in-the-loop for ratatui fullscreen mode.
//!
//! Unlike the REPL provider (which blocks on stdin), this provider stores a
//! `PendingApproval` in shared state. The TUI event loop polls this state,
//! renders an inline approval card in the chat flow, and sends the response
//! back via a oneshot channel when the user makes a choice.

use echo_agent::human_loop::{
    HumanLoopKind, HumanLoopProvider, HumanLoopRequest, HumanLoopResponse,
};
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};

/// Shared state for a pending approval request in the TUI.
pub struct PendingApproval {
    pub kind: PendingHumanLoopKind,
    /// Stable identity used to clear only the request whose future was
    /// cancelled, without racing a newer approval card.
    pub request_id: String,
    /// Tool name requesting approval.
    pub tool_name: String,
    /// Pretty-printed arguments for display.
    pub args_display: String,
    /// Risk level label (e.g. "Medium", "High").
    pub risk_label: String,
    /// The prompt text from the framework.
    pub prompt: String,
    /// Options for Selection requests.
    pub options: Vec<String>,
    /// Currently selected option index (0=同意, 1=拒绝, 2=修改, 3=全部同意).
    pub selected_option: usize,
    /// Whether we're in feedback input mode (for 拒绝/修改).
    pub input_mode: bool,
    /// The label for the current input mode ("拒绝原因" or "修改意见").
    pub input_label: String,
    /// Current feedback input text.
    pub feedback_input: String,
    /// Cursor position in feedback input (byte offset).
    pub feedback_cursor: usize,
    /// Oneshot sender to unblock the waiting agent.
    pub response_tx: Option<oneshot::Sender<HumanLoopResponse>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingHumanLoopKind {
    Approval,
    Input,
    Selection,
}

impl PendingApproval {
    /// Build a display string for risk level.
    fn risk_label(risk: Option<&echo_agent::human_loop::RiskLevel>) -> String {
        match risk {
            Some(r) => format!("{:?}", r),
            None => "Medium".to_string(),
        }
    }

    /// Number of approval options.
    pub const OPTION_COUNT: usize = 4;

    /// Option labels.
    pub const OPTION_LABELS: [&'static str; 4] =
        ["[y] 同意", "[n] 拒绝", "[m] 修改意见", "[a] 全部同意"];

    pub fn option_count(&self) -> usize {
        match self.kind {
            PendingHumanLoopKind::Approval => Self::OPTION_COUNT,
            PendingHumanLoopKind::Input => 0,
            PendingHumanLoopKind::Selection => self.options.len(),
        }
    }
}

/// TUI-based HumanLoopProvider that integrates with ratatui event loop.
pub struct TuiHumanLoopProvider {
    /// Shared pending approval state — the event loop reads this.
    pub pending: Arc<Mutex<Option<PendingApproval>>>,
}

struct PendingCleanup {
    pending: Arc<Mutex<Option<PendingApproval>>>,
    request_id: String,
}

impl Drop for PendingCleanup {
    fn drop(&mut self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let pending = self.pending.clone();
        let request_id = self.request_id.clone();
        runtime.spawn(async move {
            let mut guard = pending.lock().await;
            if guard
                .as_ref()
                .is_some_and(|approval| approval.request_id == request_id)
            {
                *guard = None;
            }
        });
    }
}

impl TuiHumanLoopProvider {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(None)),
        }
    }

    /// Get a clone of the pending state handle (for the event loop to poll).
    pub fn pending_handle(&self) -> Arc<Mutex<Option<PendingApproval>>> {
        self.pending.clone()
    }
}

impl Default for TuiHumanLoopProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl HumanLoopProvider for TuiHumanLoopProvider {
    fn request(
        &self,
        req: HumanLoopRequest,
    ) -> BoxFuture<'_, Result<HumanLoopResponse, echo_agent::error::ReactError>> {
        Box::pin(async move { self.handle_request(req).await })
    }
}

impl TuiHumanLoopProvider {
    async fn handle_request(
        &self,
        req: HumanLoopRequest,
    ) -> Result<HumanLoopResponse, echo_agent::error::ReactError> {
        let (tx, rx) = oneshot::channel();
        let request_id = uuid::Uuid::new_v4().to_string();

        let kind = match req.kind {
            HumanLoopKind::Approval => PendingHumanLoopKind::Approval,
            HumanLoopKind::Input => PendingHumanLoopKind::Input,
            HumanLoopKind::Selection => PendingHumanLoopKind::Selection,
        };
        let tool_name = req.tool_name.clone().unwrap_or_else(|| match kind {
            PendingHumanLoopKind::Approval => "unknown".to_string(),
            PendingHumanLoopKind::Input => "User input".to_string(),
            PendingHumanLoopKind::Selection => req
                .phase
                .clone()
                .or_else(|| req.task_id.clone())
                .unwrap_or_else(|| "Selection".to_string()),
        });
        let args_display = req
            .args
            .as_ref()
            .map(|a| serde_json::to_string_pretty(a).unwrap_or_default())
            .or_else(|| {
                req.context
                    .as_ref()
                    .and_then(|value| serde_json::to_string_pretty(value).ok())
            })
            .unwrap_or_default();
        let risk_label = PendingApproval::risk_label(req.risk_level.as_ref());
        let options = req.options.clone().unwrap_or_default();

        let pending = PendingApproval {
            kind,
            request_id: request_id.clone(),
            tool_name,
            args_display,
            risk_label,
            prompt: req.prompt.clone(),
            options,
            selected_option: 0,
            input_mode: kind == PendingHumanLoopKind::Input,
            input_label: if kind == PendingHumanLoopKind::Input {
                "Input".to_string()
            } else {
                String::new()
            },
            feedback_input: String::new(),
            feedback_cursor: 0,
            response_tx: Some(tx),
        };

        // Store the pending approval for the TUI event loop to pick up
        {
            let mut guard = self.pending.lock().await;
            *guard = Some(pending);
        }
        let _cleanup = PendingCleanup {
            pending: self.pending.clone(),
            request_id,
        };

        // Wait for the TUI event loop to send a response (with timeout)
        let timeout = req.timeout.unwrap_or(std::time::Duration::from_secs(300));
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => {
                // Clear pending state
                let mut guard = self.pending.lock().await;
                *guard = None;
                Ok(response)
            }
            Ok(Err(_)) => {
                // Channel dropped (shouldn't happen normally)
                let mut guard = self.pending.lock().await;
                *guard = None;
                Ok(HumanLoopResponse::Rejected {
                    reason: Some("Approval channel dropped".to_string()),
                })
            }
            Err(_) => {
                // Timeout
                let mut guard = self.pending.lock().await;
                *guard = None;
                Ok(HumanLoopResponse::Timeout)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::human_loop::HumanLoopProvider;

    #[tokio::test]
    async fn cancelling_request_clears_pending_approval() -> Result<(), String> {
        let provider = Arc::new(TuiHumanLoopProvider::new());
        let request_provider = provider.clone();
        let request = HumanLoopRequest::approval("write_file", serde_json::json!({"path": "a"}));
        let task = tokio::spawn(async move { request_provider.request(request).await });

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if provider.pending.lock().await.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "approval was not published".to_string())?;

        task.abort();
        let _ = task.await;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if provider.pending.lock().await.is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "cancelled approval was not cleared".to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn input_request_waits_for_real_text_response() -> Result<(), String> {
        let provider = Arc::new(TuiHumanLoopProvider::new());
        let request_provider = provider.clone();
        let task = tokio::spawn(async move {
            request_provider
                .request(HumanLoopRequest::input("What should change?"))
                .await
        });

        let response_tx = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let mut guard = provider.pending.lock().await;
                if let Some(pending) = guard.as_mut() {
                    if pending.kind != PendingHumanLoopKind::Input || !pending.input_mode {
                        return None;
                    }
                    return pending.response_tx.take();
                }
                drop(guard);
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "input request was not published".to_string())?
        .ok_or_else(|| "input request had no response channel".to_string())?;
        response_tx
            .send(HumanLoopResponse::Text("use the file store".to_string()))
            .map_err(|_| "failed to send input response".to_string())?;

        let response = task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        match response {
            HumanLoopResponse::Text(text) if text == "use the file store" => Ok(()),
            other => Err(format!("unexpected input response: {other:?}")),
        }
    }

    #[tokio::test]
    async fn selection_request_waits_for_selected_option() -> Result<(), String> {
        let provider = Arc::new(TuiHumanLoopProvider::new());
        let request_provider = provider.clone();
        let request = HumanLoopRequest::selection(
            "task-1",
            "Choose next step",
            vec!["Retry".to_string(), "Skip".to_string()],
        );
        let task = tokio::spawn(async move { request_provider.request(request).await });

        let response_tx = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let mut guard = provider.pending.lock().await;
                if let Some(pending) = guard.as_mut() {
                    if pending.kind != PendingHumanLoopKind::Selection
                        || pending.options != ["Retry".to_string(), "Skip".to_string()]
                    {
                        return None;
                    }
                    return pending.response_tx.take();
                }
                drop(guard);
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "selection request was not published".to_string())?
        .ok_or_else(|| "selection request had no response channel".to_string())?;
        response_tx
            .send(HumanLoopResponse::Selection {
                selection: "Skip".to_string(),
                instructions: None,
            })
            .map_err(|_| "failed to send selection response".to_string())?;

        let response = task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        match response {
            HumanLoopResponse::Selection { selection, .. } if selection == "Skip" => Ok(()),
            other => Err(format!("unexpected selection response: {other:?}")),
        }
    }
}
