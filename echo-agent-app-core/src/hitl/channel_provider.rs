//! Per-sender HumanLoopProvider for text-based IM channels.

use std::sync::Arc;

use echo_agent::human_loop::{
    ApprovalScope, HumanLoopKind, HumanLoopProvider, HumanLoopRequest, HumanLoopResponse,
};
use futures::future::BoxFuture;
use tokio::sync::{Mutex, broadcast, oneshot};

struct PendingChannelRequest {
    request_id: String,
    request: HumanLoopRequest,
    response_tx: Option<oneshot::Sender<HumanLoopResponse>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelHumanLoopResolution {
    NoPending,
    Resolved(String),
    Invalid(String),
}

pub struct ChannelHumanLoopProvider {
    pending: Arc<Mutex<Option<PendingChannelRequest>>>,
    prompt_tx: broadcast::Sender<String>,
}

impl ChannelHumanLoopProvider {
    pub fn new() -> Self {
        let (prompt_tx, _) = broadcast::channel(8);
        Self {
            pending: Arc::new(Mutex::new(None)),
            prompt_tx,
        }
    }

    pub fn subscribe_prompts(&self) -> broadcast::Receiver<String> {
        self.prompt_tx.subscribe()
    }

    pub async fn resolve_message(&self, message: &str) -> ChannelHumanLoopResolution {
        let mut pending = self.pending.lock().await;
        let Some(request) = pending.as_mut() else {
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
        let request_id = request.request_id.clone();
        let response_tx = request.response_tx.take();
        *pending = None;
        if let Some(response_tx) = response_tx
            && response_tx.send(response).is_err()
        {
            return ChannelHumanLoopResolution::Invalid(
                "The pending request already expired; send the instruction again.".to_string(),
            );
        }
        ChannelHumanLoopResolution::Resolved(format!("Resolved request {request_id}."))
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
            let request_id = uuid::Uuid::new_v4().to_string();
            let prompt = format_prompt(&request_id, &request);
            {
                let mut pending = self.pending.lock().await;
                if let Some(previous) = pending.as_mut()
                    && let Some(previous_tx) = previous.response_tx.take()
                {
                    let _ = previous_tx.send(HumanLoopResponse::Rejected {
                        reason: Some("Superseded by a newer request".to_string()),
                    });
                }
                *pending = Some(PendingChannelRequest {
                    request_id: request_id.clone(),
                    request: request.clone(),
                    response_tx: Some(response_tx),
                });
            }
            let _ = self.prompt_tx.send(prompt);

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
            let mut pending = self.pending.lock().await;
            if pending
                .as_ref()
                .is_some_and(|value| value.request_id == request_id)
            {
                *pending = None;
            }
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
}
