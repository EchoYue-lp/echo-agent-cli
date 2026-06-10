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
    /// Tool name requesting approval.
    pub tool_name: String,
    /// Pretty-printed arguments for display.
    pub args_display: String,
    /// Risk level label (e.g. "Medium", "High").
    pub risk_label: String,
    /// The prompt text from the framework.
    pub prompt: String,
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
}

/// TUI-based HumanLoopProvider that integrates with ratatui event loop.
pub struct TuiHumanLoopProvider {
    /// Shared pending approval state — the event loop reads this.
    pub pending: Arc<Mutex<Option<PendingApproval>>>,
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
        Box::pin(async move {
            match req.kind {
                HumanLoopKind::Approval => self.handle_approval(req).await,
                HumanLoopKind::Input => {
                    // For non-approval requests, auto-approve (TUI doesn't support
                    // arbitrary input dialogs yet).
                    Ok(HumanLoopResponse::Approved)
                }
                HumanLoopKind::Selection => {
                    if let Some(options) = req.options {
                        Ok(HumanLoopResponse::Selection {
                            selection: options.first().cloned().unwrap_or_default(),
                            instructions: None,
                        })
                    } else {
                        Ok(HumanLoopResponse::Approved)
                    }
                }
            }
        })
    }
}

impl TuiHumanLoopProvider {
    async fn handle_approval(
        &self,
        req: HumanLoopRequest,
    ) -> Result<HumanLoopResponse, echo_agent::error::ReactError> {
        let (tx, rx) = oneshot::channel();

        let tool_name = req
            .tool_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let args_display = req
            .args
            .as_ref()
            .map(|a| serde_json::to_string_pretty(a).unwrap_or_default())
            .unwrap_or_default();
        let risk_label = PendingApproval::risk_label(req.risk_level.as_ref());

        let pending = PendingApproval {
            tool_name,
            args_display,
            risk_label,
            prompt: req.prompt.clone(),
            selected_option: 0,
            input_mode: false,
            input_label: String::new(),
            feedback_input: String::new(),
            feedback_cursor: 0,
            response_tx: Some(tx),
        };

        // Store the pending approval for the TUI event loop to pick up
        {
            let mut guard = self.pending.lock().await;
            *guard = Some(pending);
        }

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
