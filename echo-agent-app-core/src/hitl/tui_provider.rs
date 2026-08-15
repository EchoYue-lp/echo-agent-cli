//! TUI HITL Provider — non-blocking human-in-the-loop for ratatui fullscreen mode.
//!
//! The provider stores pending requests in shared state. The TUI event loop
//! renders the front request inline and sends the response back through its
//! exact oneshot channel when the user makes a choice.

use echo_agent::error::ReactError;
use echo_agent::human_loop::{
    HumanLoopKind, HumanLoopProvider, HumanLoopRequest, HumanLoopResponse,
};
use futures::future::BoxFuture;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

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
    state: PendingApprovalQueue,
}

pub type PendingApprovalQueue = Arc<Mutex<TuiHumanLoopState>>;

pub struct TuiHumanLoopState {
    accepting: bool,
    close_reason: Option<String>,
    pending: VecDeque<PendingApproval>,
    reserved_ids: HashSet<String>,
}

impl TuiHumanLoopState {
    pub fn front(&self) -> Option<&PendingApproval> {
        self.pending.front()
    }

    pub fn front_mut(&mut self) -> Option<&mut PendingApproval> {
        self.pending.front_mut()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn resolve_front(&mut self, request_id: &str, response: HumanLoopResponse) -> bool {
        if !self
            .pending
            .front()
            .is_some_and(|approval| approval.request_id == request_id)
        {
            return false;
        }
        if let Some(response_tx) = self
            .pending
            .front_mut()
            .and_then(|approval| approval.response_tx.take())
        {
            let _ = response_tx.send(response);
        }
        self.pending.pop_front().is_some()
    }
}

struct TuiRequestReservation {
    state: PendingApprovalQueue,
    request_id: String,
    active: bool,
}

impl TuiRequestReservation {
    fn release(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(position) = state
            .pending
            .iter()
            .position(|approval| approval.request_id == self.request_id)
        {
            state.pending.remove(position);
        }
        state.reserved_ids.remove(&self.request_id);
        self.active = false;
    }
}

impl Drop for TuiRequestReservation {
    fn drop(&mut self) {
        self.release();
    }
}

/// Remove requests whose waiting future was dropped or already answered.
///
/// TUI render/input paths call this while holding the queue lock, so a
/// cancelled front request exposes the next request without a detached task.
pub fn prune_closed_pending(state: &mut TuiHumanLoopState) {
    state.pending.retain(|approval| {
        approval
            .response_tx
            .as_ref()
            .is_some_and(|sender| !sender.is_closed())
    });
}

impl TuiHumanLoopProvider {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(TuiHumanLoopState {
                accepting: true,
                close_reason: None,
                pending: VecDeque::new(),
                reserved_ids: HashSet::new(),
            })),
        }
    }

    /// Get a clone of the pending state handle (for the event loop to poll).
    pub fn pending_handle(&self) -> PendingApprovalQueue {
        self.state.clone()
    }

    /// Close admission and reject every exact request already accepted by the
    /// full-screen TUI session. Repeated calls are harmless.
    pub fn close_now(&self, reason: impl Into<String>) -> usize {
        let reason = reason.into();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.accepting = false;
        if state.close_reason.is_none() {
            state.close_reason = Some(reason.clone());
        }
        prune_closed_pending(&mut state);
        let mut rejected = 0usize;
        while let Some(mut approval) = state.pending.pop_front() {
            if approval.response_tx.take().is_some_and(|response_tx| {
                response_tx
                    .send(HumanLoopResponse::Rejected {
                        reason: Some(reason.clone()),
                    })
                    .is_ok()
            }) {
                rejected = rejected.saturating_add(1);
            }
        }
        rejected
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
    ) -> BoxFuture<'_, Result<HumanLoopResponse, ReactError>> {
        Box::pin(async move { self.handle_request(req).await })
    }
}

impl TuiHumanLoopProvider {
    async fn handle_request(&self, req: HumanLoopRequest) -> Result<HumanLoopResponse, ReactError> {
        let (tx, rx) = oneshot::channel();
        let request_id = req
            .request_id
            .clone()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

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

        // Admission, duplicate-id reservation, and queue publication share one
        // critical section, so shutdown cannot drain an empty queue and then
        // race a late request into an unobserved TUI session.
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| ReactError::Other("TUI HITL state is unavailable".to_string()))?;
            prune_closed_pending(&mut state);
            if !state.accepting {
                return Ok(HumanLoopResponse::Rejected {
                    reason: Some(
                        state
                            .close_reason
                            .clone()
                            .unwrap_or_else(|| "TUI session ended".to_string()),
                    ),
                });
            }
            if !state.reserved_ids.insert(request_id.clone()) {
                return Err(ReactError::Other(format!(
                    "duplicate TUI human-loop request id: {request_id}"
                )));
            }
            state.pending.push_back(pending);
        }
        let mut reservation = TuiRequestReservation {
            state: Arc::clone(&self.state),
            request_id: request_id.clone(),
            active: true,
        };
        // Wait for the TUI event loop to send a response (with timeout)
        let timeout = req.timeout.unwrap_or(std::time::Duration::from_secs(300));
        let response = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => HumanLoopResponse::Rejected {
                reason: Some("Approval channel dropped".to_string()),
            },
            Err(_) => HumanLoopResponse::Timeout,
        };
        reservation.release();
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::human_loop::HumanLoopProvider;

    #[tokio::test]
    async fn cancelled_request_is_pruned_without_a_cleanup_task() -> Result<(), String> {
        let provider = Arc::new(TuiHumanLoopProvider::new());
        let request_provider = provider.clone();
        let request = HumanLoopRequest::approval("write_file", serde_json::json!({"path": "a"}));
        let task = tokio::spawn(async move { request_provider.request(request).await });

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if pending_len(&provider).unwrap_or_default() > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "approval was not published".to_string())?;

        task.abort();
        let _ = task.await;
        let pending = provider.state.lock().map_err(|error| error.to_string())?;
        assert!(pending.is_empty());
        assert!(pending.reserved_ids.is_empty());
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
                let published = {
                    let mut guard = provider.state.lock().map_err(|error| error.to_string())?;
                    guard.front_mut().map(|pending| {
                        if pending.kind != PendingHumanLoopKind::Input || !pending.input_mode {
                            return None;
                        }
                        pending.response_tx.take()
                    })
                };
                if let Some(response_tx) = published {
                    return Ok::<Option<oneshot::Sender<HumanLoopResponse>>, String>(response_tx);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "input request was not published".to_string())??
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
                let published = {
                    let mut guard = provider.state.lock().map_err(|error| error.to_string())?;
                    guard.front_mut().map(|pending| {
                        if pending.kind != PendingHumanLoopKind::Selection
                            || pending.options != ["Retry".to_string(), "Skip".to_string()]
                        {
                            return None;
                        }
                        pending.response_tx.take()
                    })
                };
                if let Some(response_tx) = published {
                    return Ok::<Option<oneshot::Sender<HumanLoopResponse>>, String>(response_tx);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "selection request was not published".to_string())??
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

    #[tokio::test]
    async fn concurrent_requests_remain_fifo_and_resolve_by_exact_id() -> Result<(), String> {
        let provider = Arc::new(TuiHumanLoopProvider::new());
        let first_provider = provider.clone();
        let mut first = HumanLoopRequest::input("First");
        first.request_id = Some("request-1".to_string());
        let first_task = tokio::spawn(async move { first_provider.request(first).await });

        wait_for_pending_count(&provider, 1).await?;

        let second_provider = provider.clone();
        let mut second = HumanLoopRequest::input("Second");
        second.request_id = Some("request-2".to_string());
        let second_task = tokio::spawn(async move { second_provider.request(second).await });

        wait_for_pending_count(&provider, 2).await?;
        let first_tx = {
            let mut pending = provider.state.lock().map_err(|error| error.to_string())?;
            assert_eq!(
                pending
                    .pending
                    .iter()
                    .map(|request| request.request_id.as_str())
                    .collect::<Vec<_>>(),
                ["request-1", "request-2"]
            );
            pending
                .front_mut()
                .and_then(|request| request.response_tx.take())
                .ok_or_else(|| "first request had no response channel".to_string())?
        };
        first_tx
            .send(HumanLoopResponse::Text("first answer".to_string()))
            .map_err(|_| "failed to resolve first request".to_string())?;
        let first_response = first_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(first_response, HumanLoopResponse::Text(text) if text == "first answer"));

        wait_for_pending_count(&provider, 1).await?;
        let second_tx = {
            let mut pending = provider.state.lock().map_err(|error| error.to_string())?;
            let request = pending
                .front_mut()
                .ok_or_else(|| "second request was removed with first".to_string())?;
            assert_eq!(request.request_id, "request-2");
            request
                .response_tx
                .take()
                .ok_or_else(|| "second request had no response channel".to_string())?
        };
        second_tx
            .send(HumanLoopResponse::Text("second answer".to_string()))
            .map_err(|_| "failed to resolve second request".to_string())?;
        let second_response = second_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(
            matches!(second_response, HumanLoopResponse::Text(text) if text == "second answer")
        );
        wait_for_pending_count(&provider, 0).await
    }

    #[tokio::test]
    async fn duplicate_id_stays_reserved_until_the_first_waiter_settles() -> Result<(), String> {
        let provider = Arc::new(TuiHumanLoopProvider::new());
        let first_provider = Arc::clone(&provider);
        let mut first = HumanLoopRequest::input("First");
        first.request_id = Some("same-id".to_string());
        let first_task = tokio::spawn(async move { first_provider.request(first).await });
        wait_for_pending_count(&provider, 1).await?;

        let response_tx = {
            let mut state = provider.state.lock().map_err(|error| error.to_string())?;
            state
                .front_mut()
                .and_then(|request| request.response_tx.take())
                .ok_or_else(|| "first request response is unavailable".to_string())?
        };
        response_tx
            .send(HumanLoopResponse::Text("done".to_string()))
            .map_err(|_| "failed to resolve first request".to_string())?;

        let mut duplicate = HumanLoopRequest::input("Duplicate");
        duplicate.request_id = Some("same-id".to_string());
        let duplicate_error = provider
            .request(duplicate)
            .await
            .err()
            .ok_or_else(|| "duplicate request id was accepted".to_string())?;
        assert!(duplicate_error.to_string().contains("duplicate"));

        let first_response = first_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(first_response, HumanLoopResponse::Text(text) if text == "done"));
        Ok(())
    }

    #[tokio::test]
    async fn close_rejects_pending_and_late_requests_idempotently() -> Result<(), String> {
        let provider = Arc::new(TuiHumanLoopProvider::new());
        let first_provider = Arc::clone(&provider);
        let first_task = tokio::spawn(async move {
            first_provider
                .request(HumanLoopRequest::input("Pending"))
                .await
        });
        wait_for_pending_count(&provider, 1).await?;

        assert_eq!(provider.close_now("TUI ended"), 1);
        assert_eq!(provider.close_now("ignored repeat"), 0);
        let first_response = first_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(first_response, HumanLoopResponse::Rejected { .. }));

        let late_response = provider
            .request(HumanLoopRequest::input("Late"))
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(late_response, HumanLoopResponse::Rejected { .. }));
        assert_eq!(pending_len(&provider)?, 0);
        Ok(())
    }

    async fn wait_for_pending_count(
        provider: &TuiHumanLoopProvider,
        expected: usize,
    ) -> Result<(), String> {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if pending_len(provider).unwrap_or_default() == expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| format!("pending request count did not reach {expected}"))
    }

    fn pending_len(provider: &TuiHumanLoopProvider) -> Result<usize, String> {
        provider
            .state
            .lock()
            .map(|state| state.len())
            .map_err(|error| error.to_string())
    }
}
