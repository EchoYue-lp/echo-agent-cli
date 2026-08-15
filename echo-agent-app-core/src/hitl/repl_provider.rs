//! REPL HITL transport.
//!
//! The provider never reads stdin. It publishes pending requests to the one
//! REPL input owner and awaits an exact oneshot response.

use echo_agent::error::ReactError;
use echo_agent::human_loop::{
    ApprovalScope, HumanLoopKind, HumanLoopProvider, HumanLoopRequest, HumanLoopResponse,
};
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use tokio::sync::{mpsc, oneshot};

struct RequestIdReservation {
    request_id: String,
    state: Arc<std::sync::Mutex<ReplProviderState>>,
}

impl Drop for RequestIdReservation {
    fn drop(&mut self) {
        match self.state.lock() {
            Ok(mut state) => {
                state.in_flight.remove(&self.request_id);
            }
            Err(error) => {
                tracing::warn!(%error, "REPL HITL request-id registry is unavailable");
            }
        }
    }
}

struct PendingResponse {
    response_tx: std::sync::Mutex<Option<oneshot::Sender<HumanLoopResponse>>>,
}

impl PendingResponse {
    fn new(response_tx: oneshot::Sender<HumanLoopResponse>) -> Self {
        Self {
            response_tx: std::sync::Mutex::new(Some(response_tx)),
        }
    }

    fn is_closed(&self) -> bool {
        match self.response_tx.lock() {
            Ok(response_tx) => response_tx.as_ref().is_none_or(oneshot::Sender::is_closed),
            Err(error) => {
                tracing::warn!(%error, "REPL HITL response cell is unavailable");
                true
            }
        }
    }

    fn send(&self, response: HumanLoopResponse, request_id: &str) -> Result<(), ReactError> {
        let response_tx = self
            .response_tx
            .lock()
            .map_err(|_| ReactError::Other("REPL HITL response cell is unavailable".to_string()))?
            .take()
            .ok_or_else(|| {
                ReactError::Other(format!(
                    "REPL HITL request '{request_id}' already reached a terminal response"
                ))
            })?;
        response_tx.send(response).map_err(|_| {
            ReactError::Other(format!(
                "REPL HITL request '{request_id}' expired before response"
            ))
        })
    }
}

#[derive(Default)]
struct ReplProviderState {
    closed_reason: Option<String>,
    in_flight: HashMap<String, Weak<PendingResponse>>,
}

/// One request waiting for the REPL input broker.
pub struct PendingReplHumanLoopRequest {
    request_id: String,
    request: HumanLoopRequest,
    response: Arc<PendingResponse>,
    _reservation: Arc<RequestIdReservation>,
}

impl PendingReplHumanLoopRequest {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn is_expired(&self) -> bool {
        self.response.is_closed()
    }

    pub fn resolve(self, input: &str) -> Result<(), ReactError> {
        let response = response_for_input(&self.request, input);
        self.response.send(response, &self.request_id)
    }

    pub fn reject(self, reason: impl Into<String>) -> Result<(), ReactError> {
        self.response.send(
            HumanLoopResponse::Rejected {
                reason: Some(reason.into()),
            },
            &self.request_id,
        )
    }
}

/// Channel-backed provider owned and registered only by the REPL surface.
pub struct ReplHumanLoopProvider {
    request_tx: mpsc::UnboundedSender<PendingReplHumanLoopRequest>,
    failure_tx: mpsc::UnboundedSender<String>,
    prompt_sink: Arc<dyn Fn(String) -> Result<(), String> + Send + Sync>,
    state: Arc<std::sync::Mutex<ReplProviderState>>,
}

impl ReplHumanLoopProvider {
    pub fn channel(
        prompt_sink: Arc<dyn Fn(String) -> Result<(), String> + Send + Sync>,
    ) -> (
        Self,
        mpsc::UnboundedReceiver<PendingReplHumanLoopRequest>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (failure_tx, failure_rx) = mpsc::unbounded_channel();
        (
            Self {
                request_tx,
                failure_tx,
                prompt_sink,
                state: Arc::new(std::sync::Mutex::new(ReplProviderState::default())),
            },
            request_rx,
            failure_rx,
        )
    }

    /// Close admission and reject every exact in-flight request.
    pub fn close(&self, reason: impl Into<String>) -> Result<usize, ReactError> {
        self.close_with_reason(reason.into())
    }

    fn close_with_reason(&self, reason: String) -> Result<usize, ReactError> {
        let responses = {
            let mut state = self.state.lock().map_err(|_| {
                ReactError::Other("REPL HITL request-id registry is unavailable".to_string())
            })?;
            if state.closed_reason.is_none() {
                state.closed_reason = Some(reason.clone());
            }
            state
                .in_flight
                .iter()
                .filter_map(|(request_id, response)| {
                    response
                        .upgrade()
                        .map(|response| (request_id.clone(), response))
                })
                .collect::<Vec<_>>()
        };
        let mut rejected = 0usize;
        for (request_id, response) in responses {
            if response
                .send(
                    HumanLoopResponse::Rejected {
                        reason: Some(reason.clone()),
                    },
                    &request_id,
                )
                .is_ok()
            {
                rejected = rejected.saturating_add(1);
            }
        }
        Ok(rejected)
    }

    fn fail_prompt_sink(&self, reason: String) {
        if let Err(error) = self.close_with_reason(reason.clone()) {
            tracing::warn!(%error, "failed to reject REPL HITL requests after prompt sink failure");
        }
        let _ = self.failure_tx.send(reason);
    }
}

impl HumanLoopProvider for ReplHumanLoopProvider {
    fn request(
        &self,
        mut request: HumanLoopRequest,
    ) -> BoxFuture<'_, Result<HumanLoopResponse, ReactError>> {
        Box::pin(async move {
            let request_id = request
                .request_id
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            request.request_id = Some(request_id.clone());
            let (response_tx, response_rx) = oneshot::channel();
            let response = Arc::new(PendingResponse::new(response_tx));
            {
                let mut state = self.state.lock().map_err(|_| {
                    ReactError::Other("REPL HITL request-id registry is unavailable".to_string())
                })?;
                if let Some(reason) = state.closed_reason.as_ref() {
                    return Ok(HumanLoopResponse::Rejected {
                        reason: Some(reason.clone()),
                    });
                }
                if state.in_flight.contains_key(&request_id) {
                    return Err(ReactError::Other(format!(
                        "duplicate REPL HITL request id '{request_id}'"
                    )));
                }
                state
                    .in_flight
                    .insert(request_id.clone(), Arc::downgrade(&response));
            }
            let reservation = Arc::new(RequestIdReservation {
                request_id: request_id.clone(),
                state: Arc::clone(&self.state),
            });
            let prompt = format_request(&request_id, &request);

            self.request_tx
                .send(PendingReplHumanLoopRequest {
                    request_id: request_id.clone(),
                    request,
                    response: Arc::clone(&response),
                    _reservation: Arc::clone(&reservation),
                })
                .map_err(|_| ReactError::Other("REPL input broker is unavailable".to_string()))?;
            if let Err(error) = (self.prompt_sink)(prompt) {
                self.fail_prompt_sink(format!("REPL prompt delivery failed: {error}"));
            }
            let response = response_rx.await.map_err(|_| {
                ReactError::Other(format!("REPL input broker closed request '{request_id}'"))
            });
            drop(reservation);
            response
        })
    }
}

fn format_request(request_id: &str, request: &HumanLoopRequest) -> String {
    let mut lines = vec![format!("--- Human input required [{request_id}] ---")];
    if let Some(tool_name) = request.tool_name.as_deref() {
        lines.push(format!("Tool: {tool_name}"));
    }
    if let Some(risk) = request.risk_level.as_ref() {
        lines.push(format!("Risk: {risk:?}"));
    }
    lines.push(request.prompt.clone());
    if let Some(args) = request.args.as_ref()
        && let Ok(formatted) = serde_json::to_string_pretty(args)
    {
        lines.push(format!("Arguments:\n{formatted}"));
    }
    if let Some(options) = request.options.as_ref() {
        for (index, option) in options.iter().enumerate() {
            lines.push(format!("  [{}] {option}", index.saturating_add(1)));
        }
    }
    match &request.kind {
        HumanLoopKind::Approval => {
            lines.push(
                "Reply y, n [reason], m <feedback>, or a (approve for this session tool)."
                    .to_string(),
            );
        }
        HumanLoopKind::Input => lines.push("Reply with the requested text.".to_string()),
        HumanLoopKind::Selection => {
            lines.push("Reply with an option number or text.".to_string());
        }
    }
    lines.join("\n")
}

fn response_for_input(request: &HumanLoopRequest, input: &str) -> HumanLoopResponse {
    let trimmed = input.trim();
    match &request.kind {
        HumanLoopKind::Approval => approval_response(trimmed),
        HumanLoopKind::Input => {
            if trimmed.is_empty() {
                HumanLoopResponse::Rejected {
                    reason: Some("Empty input".to_string()),
                }
            } else {
                HumanLoopResponse::Text(trimmed.to_string())
            }
        }
        HumanLoopKind::Selection => selection_response(request, trimmed),
    }
}

fn approval_response(input: &str) -> HumanLoopResponse {
    let mut fields = input.splitn(2, char::is_whitespace);
    let command = fields.next().unwrap_or_default().to_ascii_lowercase();
    let detail = fields.next().unwrap_or_default().trim();
    if matches!(command.as_str(), "" | "y" | "yes") {
        return HumanLoopResponse::Approved;
    }
    if matches!(command.as_str(), "a" | "all") {
        return HumanLoopResponse::ApprovedWithScope {
            scope: ApprovalScope::SessionTool,
        };
    }
    if matches!(command.as_str(), "n" | "no") {
        return HumanLoopResponse::Rejected {
            reason: Some(if detail.is_empty() {
                "User rejected".to_string()
            } else {
                detail.to_string()
            }),
        };
    }
    if matches!(command.as_str(), "m" | "modify") {
        return HumanLoopResponse::Rejected {
            reason: Some(if detail.is_empty() {
                "User requested changes".to_string()
            } else {
                format!("User requested changes: {detail}")
            }),
        };
    }
    HumanLoopResponse::Rejected {
        reason: Some("User rejected".to_string()),
    }
}

fn selection_response(request: &HumanLoopRequest, input: &str) -> HumanLoopResponse {
    let selection = input
        .parse::<usize>()
        .ok()
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| {
            request
                .options
                .as_ref()
                .and_then(|options| options.get(index))
        })
        .cloned()
        .unwrap_or_else(|| input.to_string());
    HumanLoopResponse::Selection {
        selection,
        instructions: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_response_keeps_feedback_on_one_broker_line() {
        assert!(matches!(
            approval_response("m use a read-only command"),
            HumanLoopResponse::Rejected { reason: Some(reason) }
                if reason == "User requested changes: use a read-only command"
        ));
    }

    #[test]
    fn selection_uses_checked_one_based_lookup() {
        let request = HumanLoopRequest::selection(
            "task",
            "choose",
            vec!["first".to_string(), "second".to_string()],
        );
        assert!(matches!(
            selection_response(&request, "2"),
            HumanLoopResponse::Selection { selection, .. } if selection == "second"
        ));
        assert!(matches!(
            selection_response(&request, "0"),
            HumanLoopResponse::Selection { selection, .. } if selection == "0"
        ));
    }

    #[tokio::test]
    async fn concurrent_requests_keep_fifo_ids_and_exact_oneshot_responses() -> Result<(), String> {
        let (prompt_tx, prompt_rx) = std::sync::mpsc::channel();
        let (provider, mut request_rx, _failure_rx) =
            ReplHumanLoopProvider::channel(Arc::new(move |prompt| {
                prompt_tx.send(prompt).map_err(|error| error.to_string())
            }));
        let provider = Arc::new(provider);

        let mut first_request = HumanLoopRequest::input("first prompt");
        first_request.request_id = Some("first".to_string());
        let first_provider = Arc::clone(&provider);
        let first_response =
            tokio::spawn(async move { first_provider.request(first_request).await });
        let first_pending = request_rx
            .recv()
            .await
            .ok_or_else(|| "first request was not queued".to_string())?;

        let mut second_request = HumanLoopRequest::input("second prompt");
        second_request.request_id = Some("second".to_string());
        let second_provider = Arc::clone(&provider);
        let second_response =
            tokio::spawn(async move { second_provider.request(second_request).await });
        let second_pending = request_rx
            .recv()
            .await
            .ok_or_else(|| "second request was not queued".to_string())?;

        if first_pending.request_id() != "first" || second_pending.request_id() != "second" {
            return Err("request ids were not preserved in FIFO order".to_string());
        }
        first_pending
            .resolve("alpha")
            .map_err(|error| error.to_string())?;
        second_pending
            .resolve("beta")
            .map_err(|error| error.to_string())?;

        let first = first_response
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let second = second_response
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        if !matches!(first, HumanLoopResponse::Text(value) if value == "alpha") {
            return Err("first response did not reach its exact requester".to_string());
        }
        if !matches!(second, HumanLoopResponse::Text(value) if value == "beta") {
            return Err("second response did not reach its exact requester".to_string());
        }
        if prompt_rx.try_iter().count() != 2 {
            return Err("prompt sink did not receive both queued requests".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_nonempty_request_id_is_rejected_before_enqueue() -> Result<(), String> {
        let (provider, mut request_rx, _failure_rx) =
            ReplHumanLoopProvider::channel(Arc::new(|_| Ok(())));
        let provider = Arc::new(provider);

        let mut first = HumanLoopRequest::input("first");
        first.request_id = Some("duplicate-id".to_string());
        let first_provider = Arc::clone(&provider);
        let first_response = tokio::spawn(async move { first_provider.request(first).await });
        let first_pending = request_rx
            .recv()
            .await
            .ok_or_else(|| "first duplicate-id request was not queued".to_string())?;

        let mut duplicate = HumanLoopRequest::input("duplicate");
        duplicate.request_id = Some("duplicate-id".to_string());
        let duplicate_error = provider
            .request(duplicate)
            .await
            .err()
            .ok_or_else(|| "duplicate request id was accepted".to_string())?;
        if !duplicate_error.to_string().contains("duplicate-id") {
            return Err("duplicate rejection did not identify the request id".to_string());
        }
        if request_rx.try_recv().is_ok() {
            return Err("duplicate request reached the input broker".to_string());
        }

        first_pending
            .resolve("accepted")
            .map_err(|error| error.to_string())?;
        let first_result = first_response
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        if !matches!(first_result, HumanLoopResponse::Text(value) if value == "accepted") {
            return Err("original request did not retain its exact response".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn prompt_sink_failure_rejects_exact_request_and_closes_session() -> Result<(), String> {
        let (provider, mut request_rx, mut failure_rx) = ReplHumanLoopProvider::channel(Arc::new(
            |_| Err("external printer closed".to_string()),
        ));
        let provider = Arc::new(provider);
        let mut request = HumanLoopRequest::input("cannot be displayed");
        request.request_id = Some("sink-failure".to_string());

        let response = provider
            .request(request)
            .await
            .map_err(|error| error.to_string())?;
        if !matches!(
            response,
            HumanLoopResponse::Rejected { reason: Some(reason) }
                if reason.contains("external printer closed")
        ) {
            return Err("prompt sink failure did not reject the exact requester".to_string());
        }
        let failure = failure_rx
            .recv()
            .await
            .ok_or_else(|| "session failure was not published".to_string())?;
        if !failure.contains("external printer closed") {
            return Err("session failure lost the prompt sink detail".to_string());
        }
        let pending = request_rx
            .recv()
            .await
            .ok_or_else(|| "failed request never reached the session drain queue".to_string())?;
        if pending.request_id() != "sink-failure" || !pending.is_expired() {
            return Err("failed prompt did not leave one exact expired drain record".to_string());
        }

        let mut later = HumanLoopRequest::input("later");
        later.request_id = Some("after-close".to_string());
        let later = provider
            .request(later)
            .await
            .map_err(|error| error.to_string())?;
        if !matches!(later, HumanLoopResponse::Rejected { .. }) || request_rx.try_recv().is_ok() {
            return Err("closed REPL HITL session admitted a later request".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn explicit_close_rejects_all_in_flight_requests() -> Result<(), String> {
        let (provider, mut request_rx, _failure_rx) =
            ReplHumanLoopProvider::channel(Arc::new(|_| Ok(())));
        let provider = Arc::new(provider);
        let first_provider = Arc::clone(&provider);
        let second_provider = Arc::clone(&provider);
        let first = tokio::spawn(async move {
            first_provider
                .request(HumanLoopRequest::input("first"))
                .await
        });
        let second = tokio::spawn(async move {
            second_provider
                .request(HumanLoopRequest::input("second"))
                .await
        });
        let first_pending = request_rx
            .recv()
            .await
            .ok_or_else(|| "first close request was not queued".to_string())?;
        let second_pending = request_rx
            .recv()
            .await
            .ok_or_else(|| "second close request was not queued".to_string())?;
        if provider
            .close("CLI bootstrap failed")
            .map_err(|error| error.to_string())?
            != 2
        {
            return Err("session close did not reject both exact requests".to_string());
        }
        if !first_pending.is_expired() || !second_pending.is_expired() {
            return Err("closed session left a live broker request".to_string());
        }
        for response in [first, second] {
            let response = response
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
            if !matches!(
                response,
                HumanLoopResponse::Rejected { reason: Some(reason) }
                    if reason == "CLI bootstrap failed"
            ) {
                return Err("session close returned a non-rejection response".to_string());
            }
        }
        Ok(())
    }
}
