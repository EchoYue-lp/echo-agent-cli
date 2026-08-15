//! Per-sender HumanLoopProvider for text-based IM channels.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use echo_agent::human_loop::{
    ApprovalScope, HumanLoopKind, HumanLoopProvider, HumanLoopRequest, HumanLoopResponse,
};
use futures::future::BoxFuture;
use tokio::sync::{broadcast, oneshot};

struct PendingChannelRequest {
    request_id: String,
    request: HumanLoopRequest,
    response_tx: oneshot::Sender<HumanLoopResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelHumanLoopResolution {
    NoPending,
    Resolved(String),
    Invalid(String),
}

struct ChannelHumanLoopState {
    accepting: bool,
    close_reason: Option<String>,
    pending: VecDeque<PendingChannelRequest>,
    reserved_ids: HashSet<String>,
}

struct ChannelRequestReservation {
    state: Arc<Mutex<ChannelHumanLoopState>>,
    prompt_tx: broadcast::Sender<String>,
    request_id: String,
    active: bool,
}

impl ChannelRequestReservation {
    fn release(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let was_front = state
            .pending
            .front()
            .is_some_and(|request| request.request_id == self.request_id);
        if let Some(position) = state
            .pending
            .iter()
            .position(|request| request.request_id == self.request_id)
        {
            state.pending.remove(position);
        }
        state.reserved_ids.remove(&self.request_id);
        if was_front {
            ChannelHumanLoopProvider::publish_front_with(&self.prompt_tx, &state.pending);
        }
        self.active = false;
    }
}

impl Drop for ChannelRequestReservation {
    fn drop(&mut self) {
        self.release();
    }
}

pub struct ChannelHumanLoopProvider {
    state: Arc<Mutex<ChannelHumanLoopState>>,
    prompt_tx: broadcast::Sender<String>,
}

impl ChannelHumanLoopProvider {
    pub fn new() -> Self {
        let (prompt_tx, _) = broadcast::channel(8);
        Self {
            state: Arc::new(Mutex::new(ChannelHumanLoopState {
                accepting: true,
                close_reason: None,
                pending: VecDeque::new(),
                reserved_ids: HashSet::new(),
            })),
            prompt_tx,
        }
    }

    pub fn subscribe_prompts(&self) -> broadcast::Receiver<String> {
        self.prompt_tx.subscribe()
    }

    pub async fn has_pending(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if Self::prune_closed(&mut state.pending) {
            self.publish_front(&state.pending);
        }
        !state.pending.is_empty()
    }

    pub async fn reject_front(&self, reason: impl Into<String>) -> ChannelHumanLoopResolution {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::prune_closed(&mut state.pending);
        let Some(request) = state.pending.pop_front() else {
            return ChannelHumanLoopResolution::NoPending;
        };
        let request_id = request.request_id;
        let delivered = request
            .response_tx
            .send(HumanLoopResponse::Rejected {
                reason: Some(reason.into()),
            })
            .is_ok();
        self.publish_front(&state.pending);
        if delivered {
            ChannelHumanLoopResolution::Resolved(format!("Rejected request {request_id}."))
        } else {
            ChannelHumanLoopResolution::Invalid(
                "The pending request already expired; advanced to the next request.".to_string(),
            )
        }
    }

    pub async fn reject_all(&self, reason: impl Into<String>) -> usize {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::prune_closed(&mut state.pending);
        let reason = reason.into();
        let mut rejected = 0usize;
        while let Some(request) = state.pending.pop_front() {
            if request
                .response_tx
                .send(HumanLoopResponse::Rejected {
                    reason: Some(reason.clone()),
                })
                .is_ok()
            {
                rejected = rejected.saturating_add(1);
            }
        }
        rejected
    }

    /// Permanently close this per-session provider and reject every accepted
    /// request. Reservations remain live until each request future observes
    /// the rejection, so a duplicate ID cannot race channel teardown.
    pub fn close_now(&self, reason: impl Into<String>) -> usize {
        let reason = reason.into();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.accepting = false;
        state.close_reason = Some(reason.clone());
        Self::prune_closed(&mut state.pending);
        let mut rejected = 0usize;
        while let Some(request) = state.pending.pop_front() {
            if request
                .response_tx
                .send(HumanLoopResponse::Rejected {
                    reason: Some(reason.clone()),
                })
                .is_ok()
            {
                rejected = rejected.saturating_add(1);
            }
        }
        rejected
    }

    pub async fn resolve_message(&self, message: &str) -> ChannelHumanLoopResolution {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::prune_closed(&mut state.pending);
        let Some(request) = state.pending.front() else {
            return ChannelHumanLoopResolution::NoPending;
        };
        let response = match parse_response(&request.request, message) {
            Ok(response) => response,
            Err(reason) => {
                return ChannelHumanLoopResolution::Invalid(format!(
                    "{reason}\n\n{}",
                    format_prompt(&request.request_id, &request.request)
                ));
            }
        };
        let Some(request) = state.pending.pop_front() else {
            return ChannelHumanLoopResolution::NoPending;
        };
        let request_id = request.request_id;
        let delivered = request.response_tx.send(response).is_ok();
        self.publish_front(&state.pending);
        if !delivered {
            return ChannelHumanLoopResolution::Invalid(
                "The pending request already expired; advanced to the next request.".to_string(),
            );
        }
        ChannelHumanLoopResolution::Resolved(format!("Resolved request {request_id}."))
    }

    fn prune_closed(pending: &mut VecDeque<PendingChannelRequest>) -> bool {
        let previous_front = pending.front().map(|request| request.request_id.clone());
        pending.retain(|request| !request.response_tx.is_closed());
        let current_front = pending.front().map(|request| request.request_id.clone());
        previous_front != current_front
    }

    fn publish_front(&self, pending: &VecDeque<PendingChannelRequest>) {
        Self::publish_front_with(&self.prompt_tx, pending);
    }

    fn publish_front_with(
        prompt_tx: &broadcast::Sender<String>,
        pending: &VecDeque<PendingChannelRequest>,
    ) {
        if let Some(request) = pending.front() {
            let _ = prompt_tx.send(format_prompt(&request.request_id, &request.request));
        }
    }
}

impl Default for ChannelHumanLoopProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl HumanLoopProvider for ChannelHumanLoopProvider {
    fn request(
        &self,
        request: HumanLoopRequest,
    ) -> BoxFuture<'_, Result<HumanLoopResponse, echo_agent::error::ReactError>> {
        Box::pin(async move {
            if request.kind == HumanLoopKind::Selection
                && request.options.as_ref().is_none_or(Vec::is_empty)
            {
                return Ok(HumanLoopResponse::Rejected {
                    reason: Some("Selection request has no options".to_string()),
                });
            }

            let (response_tx, response_rx) = oneshot::channel();
            let request_id = request
                .request_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if Self::prune_closed(&mut state.pending) {
                    self.publish_front(&state.pending);
                }
                if !state.accepting {
                    return Err(echo_agent::error::ReactError::Other(format!(
                        "channel human-loop provider is closed: {}",
                        state
                            .close_reason
                            .as_deref()
                            .unwrap_or("channel session ended")
                    )));
                }
                if !state.reserved_ids.insert(request_id.clone()) {
                    return Err(echo_agent::error::ReactError::Other(format!(
                        "duplicate channel human-loop request id: {request_id}"
                    )));
                }
                let publish = state.pending.is_empty();
                state.pending.push_back(PendingChannelRequest {
                    request_id: request_id.clone(),
                    request: request.clone(),
                    response_tx,
                });
                if publish {
                    self.publish_front(&state.pending);
                }
            }
            let mut reservation = ChannelRequestReservation {
                state: Arc::clone(&self.state),
                prompt_tx: self.prompt_tx.clone(),
                request_id: request_id.clone(),
                active: true,
            };

            let timeout = request
                .timeout
                .unwrap_or(std::time::Duration::from_secs(300));
            let response = match tokio::time::timeout(timeout, response_rx).await {
                Ok(Ok(response)) => response,
                Ok(Err(_)) => HumanLoopResponse::Rejected {
                    reason: Some("Channel response was dropped".to_string()),
                },
                Err(_) => HumanLoopResponse::Timeout,
            };
            reservation.release();
            Ok(response)
        })
    }
}

fn parse_response(request: &HumanLoopRequest, message: &str) -> Result<HumanLoopResponse, String> {
    let value = message.trim();
    if value.eq_ignore_ascii_case("/cancel") || value == "取消" {
        return Ok(HumanLoopResponse::Rejected {
            reason: Some("Cancelled by user".to_string()),
        });
    }
    match request.kind {
        HumanLoopKind::Input => {
            if value.is_empty() {
                Err("Input cannot be empty.".to_string())
            } else {
                Ok(HumanLoopResponse::Text(value.to_string()))
            }
        }
        HumanLoopKind::Approval => parse_approval(value),
        HumanLoopKind::Selection => parse_selection(request, value),
    }
}

fn parse_approval(value: &str) -> Result<HumanLoopResponse, String> {
    let mut parts = value.splitn(2, char::is_whitespace);
    let command = parts.next().unwrap_or_default().to_ascii_lowercase();
    let detail = parts.next().map(str::trim).filter(|text| !text.is_empty());
    match command.as_str() {
        "y" | "yes" | "approve" | "同意" => Ok(HumanLoopResponse::Approved),
        "a" | "all" | "全部同意" => Ok(HumanLoopResponse::ApprovedWithScope {
            scope: ApprovalScope::SessionTool,
        }),
        "n" | "no" | "reject" | "拒绝" => Ok(HumanLoopResponse::Rejected {
            reason: detail.map(str::to_string),
        }),
        "m" | "modify" | "修改" => Ok(HumanLoopResponse::Rejected {
            reason: Some(format!(
                "Modification requested: {}",
                detail.unwrap_or("please revise the arguments")
            )),
        }),
        _ => Err("Reply y, a, n [reason], m [feedback], or /cancel.".to_string()),
    }
}

fn parse_selection(request: &HumanLoopRequest, value: &str) -> Result<HumanLoopResponse, String> {
    let options = request.options.as_deref().unwrap_or_default();
    let by_number = value
        .parse::<usize>()
        .ok()
        .and_then(|number| number.checked_sub(1))
        .and_then(|index| options.get(index));
    let selection = by_number.or_else(|| {
        options
            .iter()
            .find(|option| option.eq_ignore_ascii_case(value))
    });
    match selection {
        Some(selection) => Ok(HumanLoopResponse::Selection {
            selection: selection.clone(),
            instructions: None,
        }),
        None => Err("Reply with an option number/name, or /cancel.".to_string()),
    }
}

fn format_prompt(request_id: &str, request: &HumanLoopRequest) -> String {
    match request.kind {
        HumanLoopKind::Approval => format!(
            "[approval:{request_id}] {}\nReply y, a (all), n [reason], m [feedback], or /cancel.",
            request.prompt
        ),
        HumanLoopKind::Input => format!(
            "[input:{request_id}] {}\nReply with text, or /cancel.",
            request.prompt
        ),
        HumanLoopKind::Selection => {
            let options = request
                .options
                .as_deref()
                .unwrap_or_default()
                .iter()
                .enumerate()
                .map(|(index, option)| format!("{}. {option}", index.saturating_add(1)))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "[selection:{request_id}] {}\n{options}\nReply with an option number/name, or /cancel.",
                request.prompt
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_input_from_next_message() -> Result<(), String> {
        let provider = Arc::new(ChannelHumanLoopProvider::new());
        let mut prompts = provider.subscribe_prompts();
        let request_provider = provider.clone();
        let task = tokio::spawn(async move {
            request_provider
                .request(HumanLoopRequest::input("Describe the change"))
                .await
        });
        let prompt = tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(prompt.contains("[input:"));
        assert!(matches!(
            provider.resolve_message("Use file storage").await,
            ChannelHumanLoopResolution::Resolved(_)
        ));
        let response = task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        match response {
            HumanLoopResponse::Text(text) if text == "Use file storage" => Ok(()),
            other => Err(format!("unexpected response: {other:?}")),
        }
    }

    #[tokio::test]
    async fn selection_rejects_invalid_then_accepts_number() -> Result<(), String> {
        let provider = Arc::new(ChannelHumanLoopProvider::new());
        let mut prompts = provider.subscribe_prompts();
        let request_provider = provider.clone();
        let task = tokio::spawn(async move {
            request_provider
                .request(HumanLoopRequest::selection(
                    "task-1",
                    "Choose",
                    vec!["Retry".to_string(), "Skip".to_string()],
                ))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            provider.resolve_message("9").await,
            ChannelHumanLoopResolution::Invalid(_)
        ));
        assert!(matches!(
            provider.resolve_message("2").await,
            ChannelHumanLoopResolution::Resolved(_)
        ));
        let response = task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        match response {
            HumanLoopResponse::Selection { selection, .. } if selection == "Skip" => Ok(()),
            other => Err(format!("unexpected response: {other:?}")),
        }
    }

    #[tokio::test]
    async fn concurrent_requests_preserve_ids_and_advance_fifo() -> Result<(), String> {
        let provider = Arc::new(ChannelHumanLoopProvider::new());
        let mut prompts = provider.subscribe_prompts();
        let mut first_request = HumanLoopRequest::input("First input");
        first_request.request_id = Some("framework-first".to_string());
        let first_provider = provider.clone();
        let first = tokio::spawn(async move { first_provider.request(first_request).await });

        let first_prompt = tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "first prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(first_prompt.contains("[input:framework-first]"));

        let mut second_request = HumanLoopRequest::input("Second input");
        second_request.request_id = Some("framework-second".to_string());
        let second_provider = provider.clone();
        let second = tokio::spawn(async move { second_provider.request(second_request).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if provider
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pending
                    .len()
                    == 2
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "second request did not enter the queue".to_string())?;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), prompts.recv())
                .await
                .is_err(),
            "only the queue front may publish a prompt"
        );

        assert!(matches!(
            provider.resolve_message("first answer").await,
            ChannelHumanLoopResolution::Resolved(message)
                if message.contains("framework-first")
        ));
        let second_prompt = tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "second prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(second_prompt.contains("[input:framework-second]"));
        assert!(matches!(
            provider.resolve_message("second answer").await,
            ChannelHumanLoopResolution::Resolved(message)
                if message.contains("framework-second")
        ));

        let first_response = first
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let second_response = second
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(first_response, HumanLoopResponse::Text(text) if text == "first answer"));
        assert!(
            matches!(second_response, HumanLoopResponse::Text(text) if text == "second answer")
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_front_is_pruned_without_cleanup_task() -> Result<(), String> {
        let provider = Arc::new(ChannelHumanLoopProvider::new());
        let mut prompts = provider.subscribe_prompts();
        let mut first_request = HumanLoopRequest::input("First input");
        first_request.request_id = Some("cancelled-first".to_string());
        let first_provider = provider.clone();
        let first = tokio::spawn(async move { first_provider.request(first_request).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "first prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;

        let mut second_request = HumanLoopRequest::input("Second input");
        second_request.request_id = Some("next-second".to_string());
        let second_provider = provider.clone();
        let second = tokio::spawn(async move { second_provider.request(second_request).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if provider
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pending
                    .len()
                    == 2
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "second request did not enter the queue".to_string())?;
        first.abort();
        let _ = first.await;

        assert!(provider.has_pending().await);
        let next_prompt = tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "next prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(next_prompt.contains("[input:next-second]"));
        assert!(matches!(
            provider.reject_front("Cancelled by user").await,
            ChannelHumanLoopResolution::Resolved(message) if message.contains("next-second")
        ));
        let response = second
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            response,
            HumanLoopResponse::Rejected { reason: Some(reason) } if reason == "Cancelled by user"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_non_empty_request_id_is_rejected() -> Result<(), String> {
        let provider = Arc::new(ChannelHumanLoopProvider::new());
        let mut prompts = provider.subscribe_prompts();
        let mut first_request = HumanLoopRequest::input("First input");
        first_request.request_id = Some("same-framework-id".to_string());
        let first_provider = provider.clone();
        let first = tokio::spawn(async move { first_provider.request(first_request).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "first prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;

        let mut duplicate = HumanLoopRequest::input("Duplicate input");
        duplicate.request_id = Some("same-framework-id".to_string());
        let error = provider
            .request(duplicate)
            .await
            .err()
            .ok_or_else(|| "duplicate request id was accepted".to_string())?;
        assert!(error.to_string().contains("same-framework-id"));
        assert!(matches!(
            provider.resolve_message("first answer").await,
            ChannelHumanLoopResolution::Resolved(_)
        ));
        let first_response = first
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            first_response,
            HumanLoopResponse::Text(text) if text == "first answer"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn request_id_stays_reserved_until_original_future_settles() -> Result<(), String> {
        let provider = Arc::new(ChannelHumanLoopProvider::new());
        let mut prompts = provider.subscribe_prompts();
        let mut original = HumanLoopRequest::input("Original input");
        original.request_id = Some("reserved-until-settlement".to_string());
        let original_provider = provider.clone();
        let original_task = tokio::spawn(async move { original_provider.request(original).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "original prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;

        assert!(matches!(
            provider.resolve_message("original answer").await,
            ChannelHumanLoopResolution::Resolved(_)
        ));
        let mut duplicate = HumanLoopRequest::input("Duplicate input");
        duplicate.request_id = Some("reserved-until-settlement".to_string());
        let duplicate_error =
            provider.request(duplicate).await.err().ok_or_else(|| {
                "duplicate request entered before original settlement".to_string()
            })?;
        assert!(
            duplicate_error
                .to_string()
                .contains("reserved-until-settlement")
        );

        let response = original_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(response, HumanLoopResponse::Text(text) if text == "original answer"));
        Ok(())
    }

    #[tokio::test]
    async fn aborted_request_releases_exact_id_without_cleanup_task() -> Result<(), String> {
        let provider = Arc::new(ChannelHumanLoopProvider::new());
        let mut prompts = provider.subscribe_prompts();
        let mut first_request = HumanLoopRequest::input("First input");
        first_request.request_id = Some("reusable-after-abort".to_string());
        let first_provider = provider.clone();
        let first = tokio::spawn(async move { first_provider.request(first_request).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "first prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;
        first.abort();
        let _ = first.await;

        let mut replacement = HumanLoopRequest::input("Replacement input");
        replacement.request_id = Some("reusable-after-abort".to_string());
        let replacement_provider = provider.clone();
        let replacement_task =
            tokio::spawn(async move { replacement_provider.request(replacement).await });
        let replacement_prompt =
            tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
                .await
                .map_err(|_| "replacement prompt timeout".to_string())?
                .map_err(|error| error.to_string())?;
        assert!(replacement_prompt.contains("[input:reusable-after-abort]"));
        assert!(matches!(
            provider.resolve_message("replacement answer").await,
            ChannelHumanLoopResolution::Resolved(_)
        ));
        let response = replacement_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            response,
            HumanLoopResponse::Text(text) if text == "replacement answer"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn close_drains_accepted_requests_and_rejects_future_admission() -> Result<(), String> {
        let provider = Arc::new(ChannelHumanLoopProvider::new());
        let mut prompts = provider.subscribe_prompts();
        let mut request = HumanLoopRequest::input("Pending input");
        request.request_id = Some("closing-request".to_string());
        let request_provider = provider.clone();
        let task = tokio::spawn(async move { request_provider.request(request).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "pending prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;

        assert_eq!(provider.close_now("channel session stopped"), 1);
        let response = task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            response,
            HumanLoopResponse::Rejected { reason: Some(reason) }
                if reason == "channel session stopped"
        ));

        let error = provider
            .request(HumanLoopRequest::input("Late input"))
            .await
            .err()
            .ok_or_else(|| "closed provider accepted a new request".to_string())?;
        assert!(error.to_string().contains("channel session stopped"));
        Ok(())
    }

    #[tokio::test]
    async fn front_timeout_removes_exact_request_and_promotes_next() -> Result<(), String> {
        let provider = Arc::new(ChannelHumanLoopProvider::new());
        let mut prompts = provider.subscribe_prompts();
        let mut first_request = HumanLoopRequest::input("Expiring input");
        first_request.request_id = Some("expiring-first".to_string());
        first_request.timeout = Some(std::time::Duration::from_millis(250));
        let first_provider = provider.clone();
        let first = tokio::spawn(async move { first_provider.request(first_request).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "first prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;

        let mut second_request = HumanLoopRequest::input("Persistent input");
        second_request.request_id = Some("persistent-second".to_string());
        let second_provider = provider.clone();
        let second = tokio::spawn(async move { second_provider.request(second_request).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if provider
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pending
                    .len()
                    == 2
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "second request did not enter the queue".to_string())?;

        let first_response = first
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(first_response, HumanLoopResponse::Timeout));
        let next_prompt = tokio::time::timeout(std::time::Duration::from_secs(1), prompts.recv())
            .await
            .map_err(|_| "next prompt timeout".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(next_prompt.contains("[input:persistent-second]"));
        assert!(matches!(
            provider.resolve_message("second answer").await,
            ChannelHumanLoopResolution::Resolved(message)
                if message.contains("persistent-second")
        ));
        let second_response = second
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            second_response,
            HumanLoopResponse::Text(text) if text == "second answer"
        ));
        Ok(())
    }
}
