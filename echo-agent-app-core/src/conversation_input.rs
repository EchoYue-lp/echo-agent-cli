//! Durable conversation-input ingress shared by every interactive surface.
//!
//! [`ChatEventLog`](crate::chat_event_log::ChatEventLog) remains the only
//! journal and reducer. This service owns no queue, mailbox, executor, or
//! driver; it only applies typed facts to that existing authority and observes
//! framework-owned input receipts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::chat_event_log::{ChatEventLog, ChatEventLogError};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ConversationInputAddress")]
pub struct ConversationInputAddress {
    pub workspace_id: String,
    pub conversation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ConversationInputSource")]
#[serde(rename_all = "snake_case")]
pub enum ConversationInputSource {
    Gui,
    Tui,
    Cli,
    Channel,
}

impl ConversationInputSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gui => "gui",
            Self::Tui => "tui",
            Self::Cli => "cli",
            Self::Channel => "channel",
        }
    }
}

pub fn stable_scoped_input_id(
    address: &ConversationInputAddress,
    source: ConversationInputSource,
    external_id: &str,
) -> Result<String, ConversationInputError> {
    validate_address(address)?;
    if external_id.trim().is_empty() {
        return Err(ConversationInputError::Validation(
            "external conversation input id must not be empty".to_string(),
        ));
    }
    let canonical = echo_agent::utils::canonical_json::canonical_json_bytes(&(
        &address.workspace_id,
        &address.conversation_id,
        source.as_str(),
        external_id,
    ))
    .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
    Ok(format!(
        "conversation-input:{}:{:x}",
        source.as_str(),
        Sha256::digest(canonical)
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ConversationInputIdentity")]
pub struct ConversationInputIdentity {
    pub address: ConversationInputAddress,
    pub input_id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub payload_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ConversationInputAttempt")]
pub struct ConversationInputAttempt {
    pub identity: ConversationInputIdentity,
    pub attempt: u32,
    pub attempt_id: String,
    pub turn_id: String,
    #[serde(skip, default)]
    #[ts(skip)]
    pub observation: ConversationInputObservation,
}

#[derive(Debug, Clone, Default)]
pub struct ConversationInputObservation {
    drained: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
}

impl ConversationInputObservation {
    pub fn mark_drained(&self) {
        self.drained.store(true, Ordering::Release);
    }

    pub fn drained(&self) -> bool {
        self.drained.load(Ordering::Acquire)
    }

    pub fn mark_failed(&self) {
        self.failed.store(true, Ordering::Release);
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

impl PartialEq for ConversationInputObservation {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for ConversationInputObservation {}

impl std::hash::Hash for ConversationInputObservation {
    fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ConversationInputPhase")]
#[serde(rename_all = "snake_case")]
pub enum ConversationInputPhase {
    Persisted,
    AttemptStarted,
    MailboxAccepted,
    Drained,
    TurnSettled,
    Deferred,
    RecoveryRequired,
    Cancelled,
}

/// Framework terminal outcome of the turn that owns a conversation input.
///
/// The application keeps `ConversationInputOutcome` as a domain-facing name
/// for its persisted and TypeScript wire projection, but the Rust value is the
/// framework authority directly. There is no second EKO outcome enum or
/// lossy conversion at this boundary.
pub use echo_agent::agent::AgentSteerTurnOutcome as ConversationInputOutcome;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ConversationInputPayload")]
pub struct ConversationInputPayload {
    pub text: String,
    pub attachments: Vec<crate::types::AttachmentData>,
    #[ts(type = "number")]
    pub submitted_at_ms: u64,
    pub payload_sha256: String,
}

impl ConversationInputPayload {
    pub fn new(
        text: String,
        attachments: Vec<crate::types::AttachmentData>,
    ) -> Result<Self, ConversationInputError> {
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(ConversationInputError::Validation(
                "conversation input must contain text or attachments".to_string(),
            ));
        }
        crate::attachments::validate_attachment_batch(&attachments)
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let canonical = echo_agent::utils::canonical_json::canonical_json_bytes(&(
            text.as_str(),
            attachments.as_slice(),
        ))
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let payload_sha256 = format!("{:x}", Sha256::digest(canonical));
        Ok(Self {
            text,
            attachments,
            submitted_at_ms: echo_agent::utils::time::now_millis(),
            payload_sha256,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ConversationInputReceipt")]
pub struct ConversationInputReceipt {
    pub identity: ConversationInputIdentity,
    pub phase: ConversationInputPhase,
    pub attempt: Option<u32>,
    pub attempt_id: Option<String>,
    pub turn_id: Option<String>,
    #[ts(type = "ConversationInputOutcome | null")]
    pub outcome: Option<ConversationInputOutcome>,
    pub drained: bool,
    pub reason: Option<String>,
    pub duplicate: bool,
    #[ts(type = "number")]
    pub queue_revision: u64,
}

impl ConversationInputReceipt {
    pub fn is_dispatchable(&self) -> bool {
        matches!(
            self.phase,
            ConversationInputPhase::Persisted | ConversationInputPhase::Deferred
        ) || (self.phase == ConversationInputPhase::TurnSettled && !self.drained)
    }

    pub fn blocks_replay(&self) -> bool {
        matches!(
            self.phase,
            ConversationInputPhase::AttemptStarted
                | ConversationInputPhase::MailboxAccepted
                | ConversationInputPhase::Drained
                | ConversationInputPhase::RecoveryRequired
        ) || (self.phase == ConversationInputPhase::TurnSettled && self.drained)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ConversationInputProjection")]
pub struct ConversationInputProjection {
    pub receipt: ConversationInputReceipt,
    pub payload: ConversationInputPayload,
    #[serde(skip, default)]
    #[ts(skip)]
    pub active_attempt: Option<ConversationInputAttempt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ConversationInputFrontier")]
pub struct ConversationInputFrontier {
    #[ts(type = "number")]
    pub queue_revision: u64,
    pub items: Vec<ConversationInputProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "ConversationInputFact")]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum ConversationInputFact {
    Persisted {
        identity: ConversationInputIdentity,
        payload: ConversationInputPayload,
    },
    AttemptStarted {
        attempt: ConversationInputAttempt,
        #[ts(type = "number")]
        started_at_ms: u64,
    },
    MailboxAccepted {
        attempt: ConversationInputAttempt,
        #[ts(type = "number")]
        accepted_at_ms: u64,
    },
    Drained {
        attempt: ConversationInputAttempt,
        #[ts(type = "number")]
        drained_at_ms: u64,
    },
    TurnSettled {
        attempt: ConversationInputAttempt,
        #[ts(type = "ConversationInputOutcome")]
        outcome: ConversationInputOutcome,
        drained: bool,
        #[ts(type = "number")]
        settled_at_ms: u64,
    },
    Deferred {
        attempt: ConversationInputAttempt,
        reason: String,
        #[ts(type = "number")]
        deferred_at_ms: u64,
    },
    Reordered {
        anchor: ConversationInputIdentity,
        input_ids: Vec<String>,
        #[ts(type = "number")]
        reordered_at_ms: u64,
    },
    RecoveryRequired {
        attempt: ConversationInputAttempt,
        reason: String,
        drained: bool,
        #[ts(type = "number")]
        detected_at_ms: u64,
    },
    Cancelled {
        identity: ConversationInputIdentity,
        attempt: Option<ConversationInputAttempt>,
        drained: bool,
        reason: Option<String>,
        #[ts(type = "number")]
        cancelled_at_ms: u64,
    },
}

impl ConversationInputFact {
    pub fn identity(&self) -> &ConversationInputIdentity {
        match self {
            Self::Persisted { identity, .. } | Self::Cancelled { identity, .. } => identity,
            Self::Reordered { anchor, .. } => anchor,
            Self::AttemptStarted { attempt, .. }
            | Self::MailboxAccepted { attempt, .. }
            | Self::Drained { attempt, .. }
            | Self::TurnSettled { attempt, .. }
            | Self::Deferred { attempt, .. }
            | Self::RecoveryRequired { attempt, .. } => &attempt.identity,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConversationInputError {
    #[error("invalid conversation input: {0}")]
    Validation(String),
    #[error("conversation input id '{input_id}' already identifies different content")]
    IdCollision { input_id: String },
    #[error("conversation input revision is stale for '{input_id}'")]
    StaleRevision { input_id: String },
    #[error("conversation input attempt is stale for '{input_id}'")]
    StaleAttempt { input_id: String },
    #[error("conversation input '{input_id}' is not dispatchable")]
    NotDispatchable { input_id: String },
    #[error(transparent)]
    Journal(#[from] ChatEventLogError),
}

/// Stateless adapter over the existing `ChatEventLog` input reducer.
#[derive(Clone)]
pub struct ConversationInputService {
    log: Arc<ChatEventLog>,
}

impl ConversationInputService {
    pub fn new(log: Arc<ChatEventLog>) -> Self {
        Self { log }
    }

    pub async fn submit(
        &self,
        address: ConversationInputAddress,
        input_id: String,
        text: String,
        attachments: Vec<crate::types::AttachmentData>,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        validate_address(&address)?;
        if input_id.trim().is_empty() {
            return Err(ConversationInputError::Validation(
                "conversation input id must not be empty".to_string(),
            ));
        }
        let payload = ConversationInputPayload::new(text, attachments)?;
        self.log
            .submit_conversation_input(address, input_id, payload)
            .await
    }

    pub async fn list(
        &self,
        address: &ConversationInputAddress,
    ) -> Result<ConversationInputFrontier, ConversationInputError> {
        validate_address(address)?;
        self.log.conversation_input_frontier(address).await
    }

    pub async fn dispatch_selected(
        &self,
        identity: ConversationInputIdentity,
        expected_queue_revision: u64,
        turn_id: String,
    ) -> Result<ConversationInputProjection, ConversationInputError> {
        if turn_id.trim().is_empty() {
            return Err(ConversationInputError::Validation(
                "conversation input turn id must not be empty".to_string(),
            ));
        }
        self.log
            .start_selected_conversation_input(identity, expected_queue_revision, turn_id)
            .await
    }

    /// Project one existing foreground owner's real terminal outcome onto all
    /// non-terminal inputs tied to that exact turn. This method does not spawn
    /// or retain an observer; surfaces call it from their current foreground
    /// settlement owner.
    pub async fn settle_turn(
        &self,
        address: &ConversationInputAddress,
        turn_id: &str,
        outcome: &crate::chat_driver::TurnOutcome,
    ) -> Result<Vec<ConversationInputReceipt>, ConversationInputError> {
        validate_address(address)?;
        if turn_id.trim().is_empty() {
            return Err(ConversationInputError::Validation(
                "conversation input settlement turn id must not be empty".to_string(),
            ));
        }
        let outcome = match outcome {
            crate::chat_driver::TurnOutcome::Completed => ConversationInputOutcome::Completed,
            crate::chat_driver::TurnOutcome::Cancelled => ConversationInputOutcome::Cancelled,
            crate::chat_driver::TurnOutcome::Failed(_) => ConversationInputOutcome::Failed,
        };
        self.log
            .settle_conversation_input_turn(address, turn_id, outcome)
            .await
    }

    pub async fn settle_attempt(
        &self,
        attempt: &ConversationInputAttempt,
        outcome: &crate::chat_driver::TurnOutcome,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        let outcome = match outcome {
            crate::chat_driver::TurnOutcome::Completed => ConversationInputOutcome::Completed,
            crate::chat_driver::TurnOutcome::Cancelled => ConversationInputOutcome::Cancelled,
            crate::chat_driver::TurnOutcome::Failed(_) => ConversationInputOutcome::Failed,
        };
        self.log
            .settle_conversation_input_attempt(attempt, outcome, attempt.observation.drained())
            .await
    }

    pub async fn dispatch_next(
        &self,
        address: &ConversationInputAddress,
        turn_id: String,
    ) -> Result<Option<ConversationInputProjection>, ConversationInputError> {
        validate_address(address)?;
        if turn_id.trim().is_empty() {
            return Err(ConversationInputError::Validation(
                "conversation input turn id must not be empty".to_string(),
            ));
        }
        self.log
            .start_next_conversation_input(address, turn_id)
            .await
    }

    pub async fn cancel(
        &self,
        identity: ConversationInputIdentity,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        self.log.cancel_conversation_input(identity).await
    }

    pub async fn reorder(
        &self,
        address: &ConversationInputAddress,
        expected_queue_revision: u64,
        input_ids: Vec<String>,
    ) -> Result<u64, ConversationInputError> {
        validate_address(address)?;
        self.log
            .reorder_conversation_inputs(address, expected_queue_revision, input_ids)
            .await
    }

    pub async fn mailbox_accepted(
        &self,
        attempt: ConversationInputAttempt,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        self.append_fact(ConversationInputFact::MailboxAccepted {
            attempt,
            accepted_at_ms: echo_agent::utils::time::now_millis(),
        })
        .await
    }

    pub async fn drained(
        &self,
        attempt: ConversationInputAttempt,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        self.append_fact(ConversationInputFact::Drained {
            attempt,
            drained_at_ms: echo_agent::utils::time::now_millis(),
        })
        .await
    }

    pub async fn turn_settled(
        &self,
        attempt: ConversationInputAttempt,
        outcome: ConversationInputOutcome,
        drained: bool,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        self.append_fact(ConversationInputFact::TurnSettled {
            attempt,
            outcome,
            drained,
            settled_at_ms: echo_agent::utils::time::now_millis(),
        })
        .await
    }

    pub async fn deferred(
        &self,
        attempt: ConversationInputAttempt,
        reason: String,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        self.append_fact(ConversationInputFact::Deferred {
            attempt,
            reason,
            deferred_at_ms: echo_agent::utils::time::now_millis(),
        })
        .await
    }

    pub async fn recovery_required(
        &self,
        attempt: ConversationInputAttempt,
        reason: String,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        self.recovery_required_with_drain(attempt, reason, false)
            .await
    }

    pub async fn recovery_required_with_drain(
        &self,
        attempt: ConversationInputAttempt,
        reason: String,
        drained: bool,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        self.append_fact(ConversationInputFact::RecoveryRequired {
            attempt,
            reason,
            drained,
            detected_at_ms: echo_agent::utils::time::now_millis(),
        })
        .await
    }

    /// Fold one framework tracked-steer result into the durable ingress
    /// receipt. Explicit pre-effect rejections return to the pending frontier;
    /// an unavailable mailbox becomes recovery-required and is never replayed.
    /// A `Drained` return deliberately ends this observation. The surface
    /// adapter must retain its existing `ForegroundTurnSettlement` waiter and
    /// call [`Self::turn_settled`] for the same exact attempt.
    pub async fn observe_steer_through_drain(
        &self,
        attempt: ConversationInputAttempt,
        result: Result<echo_agent::agent::AgentSteerReceipt, echo_agent::agent::TurnSteerError>,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        let mut receipt = match result {
            Ok(receipt) => receipt,
            Err(
                error @ (echo_agent::agent::TurnSteerError::Unsupported
                | echo_agent::agent::TurnSteerError::NoActiveTurn
                | echo_agent::agent::TurnSteerError::TurnMismatch { .. }
                | echo_agent::agent::TurnSteerError::NotSteerable { .. }
                | echo_agent::agent::TurnSteerError::EmptyInput),
            ) => return self.deferred(attempt, error.to_string()).await,
            Err(error @ echo_agent::agent::TurnSteerError::StateUnavailable) => {
                return Err(self
                    .observer_failure(
                        attempt,
                        false,
                        ConversationInputError::Validation(error.to_string()),
                    )
                    .await);
            }
        };
        if receipt.turn_id() != attempt.turn_id {
            return Err(self
                .observer_failure(
                    attempt.clone(),
                    false,
                    ConversationInputError::Validation(format!(
                        "tracked steer turn mismatch: expected {}, got {}",
                        attempt.turn_id,
                        receipt.turn_id()
                    )),
                )
                .await);
        }
        if let Err(error) = self.mailbox_accepted(attempt.clone()).await {
            return Err(self.observer_failure(attempt, false, error).await);
        }
        match receipt.wait_for_drained().await {
            echo_agent::agent::AgentSteerState::Drained => {
                attempt.observation.mark_drained();
                match self.drained(attempt.clone()).await {
                    Ok(receipt) => Ok(receipt),
                    Err(error) => Err(self.observer_failure(attempt, true, error).await),
                }
            }
            echo_agent::agent::AgentSteerState::TurnSettled { outcome, drained } => {
                if drained {
                    attempt.observation.mark_drained();
                    if let Err(error) = self.drained(attempt.clone()).await {
                        return Err(self.observer_failure(attempt, true, error).await);
                    }
                }
                match self.turn_settled(attempt.clone(), outcome, drained).await {
                    Ok(receipt) => Ok(receipt),
                    Err(error) => Err(self.observer_failure(attempt, drained, error).await),
                }
            }
            echo_agent::agent::AgentSteerState::Accepted => Err(self
                .observer_failure(
                    attempt,
                    false,
                    ConversationInputError::Validation(
                        "tracked steer receipt ended before drain or settlement".to_string(),
                    ),
                )
                .await),
        }
    }

    /// Fold the initial-input receipt from the existing `AgentTurnDriver`.
    /// This is the cold-turn counterpart of
    /// [`Self::observe_steer_through_drain`]. A drained receipt still requires
    /// the existing foreground settlement adapter to call
    /// [`Self::turn_settled`].
    pub async fn observe_turn_input_through_drain(
        &self,
        attempt: ConversationInputAttempt,
        mut receipt: echo_agent::runtime::TurnInputReceipt,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        if receipt.turn_id() != attempt.turn_id {
            return Err(self
                .observer_failure(
                    attempt.clone(),
                    false,
                    ConversationInputError::Validation(format!(
                        "initial input turn mismatch: expected {}, got {}",
                        attempt.turn_id,
                        receipt.turn_id()
                    )),
                )
                .await);
        }
        let accepted = receipt.wait_for_accepted().await;
        match accepted {
            echo_agent::runtime::TurnInputState::Pending => Err(self
                .observer_failure(
                    attempt,
                    false,
                    ConversationInputError::Validation(
                        "initial input receipt remained pending".to_string(),
                    ),
                )
                .await),
            echo_agent::runtime::TurnInputState::Accepted => {
                if let Err(error) = self.mailbox_accepted(attempt.clone()).await {
                    return Err(self.observer_failure(attempt, false, error).await);
                }
                match receipt.wait_for_drained().await {
                    echo_agent::runtime::TurnInputState::Drained => {
                        attempt.observation.mark_drained();
                        match self.drained(attempt.clone()).await {
                            Ok(receipt) => Ok(receipt),
                            Err(error) => Err(self.observer_failure(attempt, true, error).await),
                        }
                    }
                    echo_agent::runtime::TurnInputState::TurnSettled { outcome, drained } => {
                        if drained {
                            attempt.observation.mark_drained();
                            if let Err(error) = self.drained(attempt.clone()).await {
                                return Err(self.observer_failure(attempt, true, error).await);
                            }
                        }
                        match self.turn_settled(attempt.clone(), outcome, drained).await {
                            Ok(receipt) => Ok(receipt),
                            Err(error) => Err(self.observer_failure(attempt, drained, error).await),
                        }
                    }
                    echo_agent::runtime::TurnInputState::Pending
                    | echo_agent::runtime::TurnInputState::Accepted => Err(self
                        .observer_failure(
                            attempt,
                            false,
                            ConversationInputError::Validation(
                                "initial input receipt ended before drain or settlement"
                                    .to_string(),
                            ),
                        )
                        .await),
                }
            }
            echo_agent::runtime::TurnInputState::Drained => {
                attempt.observation.mark_drained();
                if let Err(error) = self.mailbox_accepted(attempt.clone()).await {
                    return Err(self.observer_failure(attempt, true, error).await);
                }
                match self.drained(attempt.clone()).await {
                    Ok(receipt) => Ok(receipt),
                    Err(error) => Err(self.observer_failure(attempt, true, error).await),
                }
            }
            echo_agent::runtime::TurnInputState::TurnSettled { outcome, drained } => {
                if drained {
                    attempt.observation.mark_drained();
                    if let Err(error) = self.mailbox_accepted(attempt.clone()).await {
                        return Err(self.observer_failure(attempt, true, error).await);
                    }
                    if let Err(error) = self.drained(attempt.clone()).await {
                        return Err(self.observer_failure(attempt, true, error).await);
                    }
                }
                match self.turn_settled(attempt.clone(), outcome, drained).await {
                    Ok(receipt) => Ok(receipt),
                    Err(error) => Err(self.observer_failure(attempt, drained, error).await),
                }
            }
        }
    }

    async fn observer_failure(
        &self,
        attempt: ConversationInputAttempt,
        drained: bool,
        error: ConversationInputError,
    ) -> ConversationInputError {
        attempt.observation.mark_failed();
        let reason = format!("conversation input receipt persistence failed: {error}");
        match self
            .recovery_required_with_drain(attempt, reason.clone(), drained)
            .await
        {
            Ok(_) => ConversationInputError::Validation(reason),
            Err(recovery) => ConversationInputError::Validation(format!(
                "{reason}; recovery-required persistence also failed: {recovery}"
            )),
        }
    }

    async fn append_fact(
        &self,
        fact: ConversationInputFact,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        self.log.append_conversation_input_fact(fact).await
    }
}

fn validate_address(address: &ConversationInputAddress) -> Result<(), ConversationInputError> {
    if address.workspace_id.trim().is_empty() || address.conversation_id.trim().is_empty() {
        return Err(ConversationInputError::Validation(
            "workspace and conversation ids must not be empty".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_event_log::ChatEventRetention;

    fn address() -> ConversationInputAddress {
        ConversationInputAddress {
            workspace_id: "workspace-input".to_string(),
            conversation_id: "conversation-input".to_string(),
        }
    }

    fn exact_attempt(
        projection: &ConversationInputProjection,
    ) -> Result<ConversationInputAttempt, String> {
        Ok(ConversationInputAttempt {
            identity: projection.receipt.identity.clone(),
            attempt: projection
                .receipt
                .attempt
                .ok_or_else(|| "attempt ordinal is missing".to_string())?,
            attempt_id: projection
                .receipt
                .attempt_id
                .clone()
                .ok_or_else(|| "attempt id is missing".to_string())?,
            turn_id: projection
                .receipt
                .turn_id
                .clone()
                .ok_or_else(|| "attempt turn id is missing".to_string())?,
            observation: Default::default(),
        })
    }

    fn phase_count(
        log: &ChatEventLog,
        address: &ConversationInputAddress,
        input_id: &str,
        phase: ConversationInputPhase,
    ) -> Result<usize, String> {
        let replay = log
            .replay(
                &address.workspace_id,
                Some(&address.conversation_id),
                &address.conversation_id,
                0,
            )
            .map_err(|error| error.to_string())?;
        Ok(replay
            .events
            .iter()
            .filter(|envelope| {
                matches!(
                    &envelope.payload,
                    crate::chat_driver::ChatDriverEvent::InputLifecycle(fact)
                        if fact.identity().input_id == input_id
                            && matches!((fact.as_ref(), phase),
                                (ConversationInputFact::Persisted { .. }, ConversationInputPhase::Persisted)
                                | (ConversationInputFact::AttemptStarted { .. }, ConversationInputPhase::AttemptStarted)
                                | (ConversationInputFact::MailboxAccepted { .. }, ConversationInputPhase::MailboxAccepted)
                                | (ConversationInputFact::Drained { .. }, ConversationInputPhase::Drained)
                                | (ConversationInputFact::TurnSettled { .. }, ConversationInputPhase::TurnSettled)
                                | (ConversationInputFact::Deferred { .. }, ConversationInputPhase::Deferred)
                                | (ConversationInputFact::RecoveryRequired { .. }, ConversationInputPhase::RecoveryRequired)
                                | (ConversationInputFact::Cancelled { .. }, ConversationInputPhase::Cancelled)
                            )
                )
            })
            .count())
    }

    #[test]
    fn scoped_input_id_is_stable_and_source_isolated() -> Result<(), String> {
        let gui = stable_scoped_input_id(&address(), ConversationInputSource::Gui, "external-1")
            .map_err(|error| error.to_string())?;
        let same = stable_scoped_input_id(&address(), ConversationInputSource::Gui, "external-1")
            .map_err(|error| error.to_string())?;
        let channel =
            stable_scoped_input_id(&address(), ConversationInputSource::Channel, "external-1")
                .map_err(|error| error.to_string())?;
        assert_eq!(gui, same);
        assert_ne!(gui, channel);
        assert!(stable_scoped_input_id(&address(), ConversationInputSource::Tui, "").is_err());
        Ok(())
    }

    #[tokio::test]
    async fn submit_is_idempotent_and_conflicting_payload_fails_closed() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let service = ConversationInputService::new(Arc::clone(&log));
        let first = service
            .submit(
                address(),
                "same-input".to_string(),
                "continue".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let duplicate = service
            .submit(
                address(),
                "same-input".to_string(),
                "continue".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(first.identity, duplicate.identity.clone());
        assert!(duplicate.duplicate);
        assert!(matches!(
            service
                .submit(
                    address(),
                    "same-input".to_string(),
                    "different".to_string(),
                    Vec::new(),
                )
                .await,
            Err(ConversationInputError::IdCollision { .. })
        ));
        assert_eq!(
            service
                .list(&address())
                .await
                .map_err(|e| e.to_string())?
                .items
                .len(),
            1
        );
        assert!(matches!(
            log.append(
                &address().workspace_id,
                Some(&address().conversation_id),
                &first.identity.input_id,
                crate::chat_driver::ChatDriverEvent::InputLifecycle(Box::new(
                    ConversationInputFact::Cancelled {
                        identity: first.identity.clone(),
                        attempt: None,
                        drained: false,
                        reason: None,
                        cancelled_at_ms: echo_agent::utils::time::now_millis(),
                    },
                )),
            ),
            Err(ChatEventLogError::InvalidEvent(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn two_handles_linearize_submit_and_dispatch() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("shared-input-log");
        let log_a = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let log_b = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let service_a = ConversationInputService::new(Arc::clone(&log_a));
        let service_b = ConversationInputService::new(log_b);
        let submit_a = service_a.submit(
            address(),
            "concurrent-same".to_string(),
            "same payload".to_string(),
            Vec::new(),
        );
        let submit_b = service_b.submit(
            address(),
            "concurrent-same".to_string(),
            "same payload".to_string(),
            Vec::new(),
        );
        let (first, second) = tokio::join!(submit_a, submit_b);
        let first = first.map_err(|error| error.to_string())?;
        let second = second.map_err(|error| error.to_string())?;
        assert_ne!(first.duplicate, second.duplicate);
        assert_eq!(
            phase_count(
                log_a.as_ref(),
                &address(),
                "concurrent-same",
                ConversationInputPhase::Persisted,
            )?,
            1
        );

        let target = address();
        let dispatch_a = service_a.dispatch_next(&target, "turn-one".to_string());
        let dispatch_b = service_b.dispatch_next(&target, "turn-two".to_string());
        let (first_dispatch, second_dispatch) = tokio::join!(dispatch_a, dispatch_b);
        let started = usize::from(first_dispatch.map_err(|error| error.to_string())?.is_some())
            + usize::from(
                second_dispatch
                    .map_err(|error| error.to_string())?
                    .is_some(),
            );
        assert_eq!(started, 1);
        assert_eq!(
            phase_count(
                log_a.as_ref(),
                &address(),
                "concurrent-same",
                ConversationInputPhase::AttemptStarted,
            )?,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn two_handles_reject_conflicting_concurrent_payloads() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("conflicting-input-log");
        let service_a = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let service_b = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let (first, second) = tokio::join!(
            service_a.submit(
                address(),
                "conflicting-input".to_string(),
                "payload a".to_string(),
                Vec::new(),
            ),
            service_b.submit(
                address(),
                "conflicting-input".to_string(),
                "payload b".to_string(),
                Vec::new(),
            )
        );
        assert!(matches!(
            (&first, &second),
            (Ok(_), Err(ConversationInputError::IdCollision { .. }))
                | (Err(ConversationInputError::IdCollision { .. }), Ok(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_selected_is_atomic_for_non_head_identity_and_queue_token()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let service = ConversationInputService::new(Arc::clone(&log));
        service
            .submit(
                address(),
                "selected-head".to_string(),
                "head".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        service
            .submit(
                address(),
                "selected-second".to_string(),
                "second".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let frontier = service.list(&address()).await.map_err(|e| e.to_string())?;
        let selected = frontier
            .items
            .get(1)
            .ok_or_else(|| "non-head input is missing".to_string())?;
        let before_attempts = phase_count(
            log.as_ref(),
            &address(),
            "selected-second",
            ConversationInputPhase::AttemptStarted,
        )?;
        assert!(matches!(
            service
                .dispatch_selected(
                    selected.receipt.identity.clone(),
                    frontier.queue_revision.saturating_sub(1),
                    "stale-token-turn".to_string(),
                )
                .await,
            Err(ConversationInputError::StaleRevision { .. })
        ));
        let mut stale_identity = selected.receipt.identity.clone();
        stale_identity.payload_sha256 = "different-payload-hash".to_string();
        assert!(matches!(
            service
                .dispatch_selected(
                    stale_identity,
                    frontier.queue_revision,
                    "stale-identity-turn".to_string(),
                )
                .await,
            Err(ConversationInputError::StaleRevision { .. })
        ));
        assert_eq!(
            phase_count(
                log.as_ref(),
                &address(),
                "selected-second",
                ConversationInputPhase::AttemptStarted,
            )?,
            before_attempts
        );
        let started = service
            .dispatch_selected(
                selected.receipt.identity.clone(),
                frontier.queue_revision,
                "selected-turn".to_string(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(started.receipt.identity.input_id, "selected-second");
        assert_eq!(
            started.receipt.phase,
            ConversationInputPhase::AttemptStarted
        );
        assert!(
            service
                .dispatch_next(&address(), "must-block".to_string())
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert_eq!(
            phase_count(
                log.as_ref(),
                &address(),
                "selected-head",
                ConversationInputPhase::AttemptStarted,
            )?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn foreground_owner_settles_every_exact_turn_input_once() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let service = ConversationInputService::new(Arc::clone(&log));
        for input_id in ["same-turn-a", "same-turn-b"] {
            service
                .submit(
                    address(),
                    input_id.to_string(),
                    input_id.to_string(),
                    Vec::new(),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        for _ in 0..2 {
            let started = service
                .dispatch_next(&address(), "shared-foreground-turn".to_string())
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "shared-turn input was not dispatched".to_string())?;
            let attempt = exact_attempt(&started)?;
            service
                .mailbox_accepted(attempt.clone())
                .await
                .map_err(|error| error.to_string())?;
            service
                .drained(attempt)
                .await
                .map_err(|error| error.to_string())?;
        }
        let settled = service
            .settle_turn(
                &address(),
                "shared-foreground-turn",
                &crate::chat_driver::TurnOutcome::Completed,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(settled.len(), 2);
        assert!(settled.iter().all(|receipt| {
            receipt.phase == ConversationInputPhase::TurnSettled
                && receipt.outcome == Some(ConversationInputOutcome::Completed)
                && receipt.drained
                && receipt.turn_id.as_deref() == Some("shared-foreground-turn")
        }));
        for input_id in ["same-turn-a", "same-turn-b"] {
            assert_eq!(
                phase_count(
                    log.as_ref(),
                    &address(),
                    input_id,
                    ConversationInputPhase::TurnSettled,
                )?,
                1
            );
        }
        assert!(
            service
                .settle_turn(
                    &address(),
                    "shared-foreground-turn",
                    &crate::chat_driver::TurnOutcome::Completed,
                )
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn foreground_owner_settles_attempt_started_as_undrained_fifo_head() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        service
            .submit(
                address(),
                "undrained-owner".to_string(),
                "undrained owner".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        service
            .dispatch_next(&address(), "undrained-owner-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "undrained owner attempt is missing".to_string())?;
        let settled = service
            .settle_turn(
                &address(),
                "undrained-owner-turn",
                &crate::chat_driver::TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "test",
                    "undrained",
                )),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(settled.len(), 1);
        assert!(!settled.first().is_some_and(|receipt| receipt.drained));
        let frontier = service.list(&address()).await.map_err(|e| e.to_string())?;
        assert_eq!(
            frontier
                .items
                .first()
                .map(|item| item.receipt.identity.input_id.as_str()),
            Some("undrained-owner")
        );
        assert_eq!(
            frontier.items.first().map(|item| item.receipt.phase),
            Some(ConversationInputPhase::TurnSettled)
        );
        Ok(())
    }

    #[tokio::test]
    async fn foreground_terminal_closes_observer_recovery_and_unblocks_next_fifo()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        for input_id in ["observer-failed", "observer-next"] {
            service
                .submit(
                    address(),
                    input_id.to_string(),
                    input_id.to_string(),
                    Vec::new(),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        let started = service
            .dispatch_next(&address(), "observer-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "observer failure attempt is missing".to_string())?;
        service
            .recovery_required(
                exact_attempt(&started)?,
                "runtime observer persistence failed".to_string(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let settled = service
            .settle_turn(
                &address(),
                "observer-turn",
                &crate::chat_driver::TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "observer", "failed",
                )),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(settled.len(), 1);
        let cancelled = settled
            .first()
            .ok_or_else(|| "observer failure terminal is missing".to_string())?;
        assert_eq!(cancelled.phase, ConversationInputPhase::Cancelled);
        assert!(
            cancelled
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("observer persistence failed"))
        );
        let next = service
            .dispatch_next(&address(), "next-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "next FIFO item remained blocked".to_string())?;
        assert_eq!(next.receipt.identity.input_id, "observer-next");
        Ok(())
    }

    #[tokio::test]
    async fn failed_observer_without_recovery_fact_never_requeues_ambiguous_attempt()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("failed-observer-terminal");
        let log = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let service = ConversationInputService::new(Arc::clone(&log));
        service
            .submit(
                address(),
                "ambiguous-observer".to_string(),
                "must not replay".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let projection = service
            .dispatch_next(&address(), "ambiguous-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "ambiguous observer attempt is missing".to_string())?;
        let attempt = exact_attempt(&projection)?;
        attempt.observation.mark_failed();
        let terminal = service
            .settle_attempt(
                &attempt,
                &crate::chat_driver::TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "input_observer",
                    "double append",
                )),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(terminal.phase, ConversationInputPhase::Cancelled);
        assert!(
            service
                .list(&address())
                .await
                .map_err(|e| e.to_string())?
                .items
                .is_empty()
        );
        drop(service);
        drop(log);

        let reopened = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        assert!(
            reopened
                .list(&address())
                .await
                .map_err(|e| e.to_string())?
                .items
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn stale_attempt_cannot_cross_deferred_aba_generation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("aba-log");
        let log = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let service = ConversationInputService::new(Arc::clone(&log));
        let second_handle = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        service
            .submit(
                address(),
                "aba-input".to_string(),
                "retry exactly".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let first = service
            .dispatch_next(&address(), "turn-a".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "first dispatch is missing".to_string())?;
        let stale = exact_attempt(&first)?;
        service
            .deferred(stale.clone(), "safe pre-effect rejection".to_string())
            .await
            .map_err(|error| error.to_string())?;
        let second = service
            .dispatch_next(&address(), "turn-b".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "second dispatch is missing".to_string())?;
        let current = exact_attempt(&second)?;
        assert_eq!(current.attempt, stale.attempt.saturating_add(1));
        assert!(matches!(
            second_handle.mailbox_accepted(stale).await,
            Err(ConversationInputError::StaleAttempt { .. })
        ));
        let accepted = service
            .mailbox_accepted(current.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(!accepted.duplicate);
        let duplicate = second_handle
            .mailbox_accepted(current.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        assert_eq!(
            phase_count(
                log.as_ref(),
                &address(),
                "aba-input",
                ConversationInputPhase::MailboxAccepted,
            )?,
            1
        );
        service
            .drained(current)
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            service
                .list(&address())
                .await
                .map_err(|e| e.to_string())?
                .items
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn attempt_started_reopens_as_non_replayable_until_reconciled() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("input-log");
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        service
            .submit(
                address(),
                "started-input".to_string(),
                "do not replay ambiguously".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let started = service
            .dispatch_next(&address(), "started-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "started attempt is missing".to_string())?;
        drop(service);

        let reopened = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let recovered = reopened.list(&address()).await.map_err(|e| e.to_string())?;
        assert_eq!(recovered.items.len(), 1);
        assert_eq!(
            recovered.items.first().map(|item| item.receipt.phase),
            Some(ConversationInputPhase::AttemptStarted)
        );
        assert!(
            reopened
                .dispatch_next(&address(), "must-not-start".to_string())
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        let attempt = exact_attempt(&started)?;
        reopened
            .recovery_required(attempt, "owner disappeared".to_string())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .list(&address())
                .await
                .map_err(|error| error.to_string())?
                .items
                .first()
                .map(|item| item.receipt.phase),
            Some(ConversationInputPhase::RecoveryRequired)
        );
        Ok(())
    }

    #[tokio::test]
    async fn drained_input_never_reenters_frontier_after_reopen() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("drained-log");
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        service
            .submit(
                address(),
                "drained-input".to_string(),
                "consume once".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let projection = service
            .dispatch_next(&address(), "drained-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "drain attempt is missing".to_string())?;
        let attempt = exact_attempt(&projection)?;
        service
            .mailbox_accepted(attempt.clone())
            .await
            .map_err(|error| error.to_string())?;
        service
            .drained(attempt)
            .await
            .map_err(|error| error.to_string())?;
        drop(service);

        let reopened = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        assert!(
            reopened
                .list(&address())
                .await
                .map_err(|e| e.to_string())?
                .items
                .is_empty()
        );
        assert!(
            reopened
                .dispatch_next(&address(), "replay-turn".to_string())
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn undrained_settlement_requeues_at_fifo_head_and_reorder_is_cas() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = ConversationInputService::new(Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let first = service
            .submit(
                address(),
                "fifo-first".to_string(),
                "first".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let second = service
            .submit(
                address(),
                "fifo-second".to_string(),
                "second".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let queue_revision = first.queue_revision.max(second.queue_revision);
        assert!(matches!(
            service
                .reorder(
                    &address(),
                    queue_revision,
                    vec!["fifo-first".to_string(), "fifo-first".to_string()],
                )
                .await,
            Err(ConversationInputError::Validation(_))
        ));
        let reordered_revision = service
            .reorder(
                &address(),
                queue_revision,
                vec!["fifo-second".to_string(), "fifo-first".to_string()],
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            service
                .reorder(
                    &address(),
                    queue_revision,
                    vec!["fifo-first".to_string(), "fifo-second".to_string()],
                )
                .await,
            Err(ConversationInputError::StaleRevision { .. })
        ));
        assert!(reordered_revision > queue_revision);
        let reordered = service.list(&address()).await.map_err(|e| e.to_string())?;
        assert!(
            reordered
                .items
                .iter()
                .all(|item| item.receipt.queue_revision == reordered_revision)
        );

        let started = service
            .dispatch_next(&address(), "undrained-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "reordered head was not dispatched".to_string())?;
        assert_eq!(started.receipt.identity.input_id, "fifo-second");
        let attempt = exact_attempt(&started)?;
        service
            .turn_settled(attempt, ConversationInputOutcome::Failed, false)
            .await
            .map_err(|error| error.to_string())?;
        let retry = service
            .dispatch_next(&address(), "undrained-retry".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "undrained head was not retried".to_string())?;
        assert_eq!(retry.receipt.identity.input_id, "fifo-second");
        assert_eq!(retry.receipt.attempt, Some(2));
        let latest_queue_revision = retry.receipt.queue_revision;
        assert!(latest_queue_revision > reordered_revision);

        drop(service);
        let reopened = ConversationInputService::new(Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        assert!(
            reopened
                .list(&address())
                .await
                .map_err(|error| error.to_string())?
                .items
                .iter()
                .all(|item| item.receipt.queue_revision == latest_queue_revision)
        );
        Ok(())
    }

    #[tokio::test]
    async fn active_cancel_is_rejected_while_drain_wins_exactly_once() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("cancel-drain-log");
        let service_a = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let service_b = ConversationInputService::new(Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ));
        let submitted = service_a
            .submit(
                address(),
                "cancel-active".to_string(),
                "active input".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let started = service_a
            .dispatch_next(&address(), "cancel-active-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "active cancel attempt is missing".to_string())?;
        let attempt = exact_attempt(&started)?;
        assert!(matches!(
            service_b.cancel(submitted.identity.clone()).await,
            Err(ConversationInputError::NotDispatchable { .. })
        ));
        service_a
            .mailbox_accepted(attempt.clone())
            .await
            .map_err(|error| error.to_string())?;
        let (cancel, drain) = tokio::join!(
            service_b.cancel(submitted.identity),
            service_a.drained(attempt)
        );
        assert!(matches!(
            cancel,
            Err(ConversationInputError::NotDispatchable { .. })
        ));
        assert_eq!(
            drain.map_err(|error| error.to_string())?.phase,
            ConversationInputPhase::Drained
        );
        Ok(())
    }

    #[tokio::test]
    async fn empty_dispatch_is_none_and_terminal_tombstone_survives_prune() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("terminal-tombstone-log");
        let retention = ChatEventRetention {
            segment_rollover_bytes: 128,
            max_segments: 2,
            max_replay_events: 4096,
        };
        let log =
            Arc::new(ChatEventLog::open(&root, retention).map_err(|error| error.to_string())?);
        let service = ConversationInputService::new(Arc::clone(&log));
        let empty = service.list(&address()).await.map_err(|e| e.to_string())?;
        assert_eq!(empty.queue_revision, 0);
        assert!(empty.items.is_empty());
        assert!(
            service
                .dispatch_next(&address(), "empty-turn".to_string())
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        service
            .submit(
                address(),
                "terminal-input".to_string(),
                "terminal payload".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let started = service
            .dispatch_next(&address(), "terminal-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "terminal attempt is missing".to_string())?;
        let attempt = exact_attempt(&started)?;
        service
            .mailbox_accepted(attempt.clone())
            .await
            .map_err(|error| error.to_string())?;
        service
            .drained(attempt.clone())
            .await
            .map_err(|error| error.to_string())?;
        service
            .turn_settled(attempt, ConversationInputOutcome::Completed, true)
            .await
            .map_err(|error| error.to_string())?;
        let cancelled = service
            .submit(
                address(),
                "cancelled-input".to_string(),
                "cancelled payload".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        service
            .cancel(cancelled.identity)
            .await
            .map_err(|error| error.to_string())?;
        let deferred_cancel = service
            .submit(
                address(),
                "deferred-cancelled-input".to_string(),
                "deferred cancelled payload".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let deferred_started = service
            .dispatch_next(&address(), "deferred-cancel-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "deferred cancel attempt is missing".to_string())?;
        let deferred_attempt = exact_attempt(&deferred_started)?;
        service
            .deferred(
                deferred_attempt.clone(),
                "safe to cancel after rejection".to_string(),
            )
            .await
            .map_err(|error| error.to_string())?;
        service
            .cancel(deferred_cancel.identity)
            .await
            .map_err(|error| error.to_string())?;
        for index in 0..32_u32 {
            log.append(
                &address().workspace_id,
                Some(&address().conversation_id),
                &format!("retention-noise-{index}"),
                crate::chat_driver::ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        drop(service);
        drop(log);

        let reopened_log =
            Arc::new(ChatEventLog::open(&root, retention).map_err(|error| error.to_string())?);
        assert_eq!(
            phase_count(
                reopened_log.as_ref(),
                &address(),
                "terminal-input",
                ConversationInputPhase::Persisted,
            )?,
            0,
            "fixture must prune the original Persisted fact"
        );
        let reopened = ConversationInputService::new(reopened_log);
        let duplicate = reopened
            .submit(
                address(),
                "terminal-input".to_string(),
                "terminal payload".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.phase, ConversationInputPhase::TurnSettled);
        assert!(duplicate.drained);
        assert!(matches!(
            reopened
                .submit(
                    address(),
                    "terminal-input".to_string(),
                    "different terminal payload".to_string(),
                    Vec::new(),
                )
                .await,
            Err(ConversationInputError::IdCollision { .. })
        ));
        let cancelled_duplicate = reopened
            .submit(
                address(),
                "cancelled-input".to_string(),
                "cancelled payload".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(cancelled_duplicate.duplicate);
        assert_eq!(cancelled_duplicate.phase, ConversationInputPhase::Cancelled);
        let deferred_duplicate = reopened
            .submit(
                address(),
                "deferred-cancelled-input".to_string(),
                "deferred cancelled payload".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(deferred_duplicate.duplicate);
        assert_eq!(deferred_duplicate.phase, ConversationInputPhase::Cancelled);
        assert_eq!(deferred_duplicate.attempt, Some(deferred_attempt.attempt));
        assert_eq!(
            deferred_duplicate.attempt_id.as_deref(),
            Some(deferred_attempt.attempt_id.as_str())
        );
        assert_eq!(
            deferred_duplicate.turn_id.as_deref(),
            Some(deferred_attempt.turn_id.as_str())
        );
        Ok(())
    }

    #[tokio::test]
    async fn boot_reconcile_blocks_unknown_attempt_and_terminalizes_drained_owner_loss()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("boot-input-log");
        let log = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let service = ConversationInputService::new(Arc::clone(&log));
        for input_id in ["boot-unknown", "boot-drained"] {
            service
                .submit(
                    address(),
                    input_id.to_string(),
                    input_id.to_string(),
                    Vec::new(),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        let unknown = service
            .dispatch_selected(
                service
                    .list(&address())
                    .await
                    .map_err(|error| error.to_string())?
                    .items
                    .first()
                    .ok_or_else(|| "boot unknown input is missing".to_string())?
                    .receipt
                    .identity
                    .clone(),
                service
                    .list(&address())
                    .await
                    .map_err(|error| error.to_string())?
                    .queue_revision,
                "boot-unknown-turn".to_string(),
            )
            .await
            .map_err(|error| error.to_string())?;
        service
            .deferred(exact_attempt(&unknown)?, "allow next fixture".to_string())
            .await
            .map_err(|error| error.to_string())?;
        let frontier = service.list(&address()).await.map_err(|e| e.to_string())?;
        let drained_identity = frontier
            .items
            .iter()
            .find(|item| item.receipt.identity.input_id == "boot-drained")
            .ok_or_else(|| "boot drained input is missing".to_string())?
            .receipt
            .identity
            .clone();
        let drained = service
            .dispatch_selected(
                drained_identity,
                frontier.queue_revision,
                "boot-drained-turn".to_string(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let drained_attempt = exact_attempt(&drained)?;
        service
            .mailbox_accepted(drained_attempt.clone())
            .await
            .map_err(|error| error.to_string())?;
        service
            .drained(drained_attempt)
            .await
            .map_err(|error| error.to_string())?;
        let unknown_retry = service
            .dispatch_selected(
                unknown.receipt.identity.clone(),
                service
                    .list(&address())
                    .await
                    .map_err(|error| error.to_string())?
                    .queue_revision,
                "boot-unknown-turn-2".to_string(),
            )
            .await
            .map_err(|error| error.to_string())?;
        drop(service);

        assert_eq!(
            log.reconcile_conversation_inputs_at_boot()
                .map_err(|error| error.to_string())?,
            2
        );
        let reopened = ConversationInputService::new(log);
        let frontier = reopened.list(&address()).await.map_err(|e| e.to_string())?;
        assert!(frontier.items.is_empty());
        let cancelled = reopened
            .submit(
                address(),
                "boot-unknown".to_string(),
                "boot-unknown".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(cancelled.phase, ConversationInputPhase::Cancelled);
        assert!(!cancelled.drained);
        assert!(
            cancelled
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("owner was lost"))
        );
        assert_eq!(cancelled.attempt, unknown_retry.receipt.attempt);
        assert!(
            reopened
                .submit(
                    address(),
                    "boot-drained".to_string(),
                    "boot-drained".to_string(),
                    Vec::new(),
                )
                .await
                .map_err(|error| error.to_string())?
                .drained
        );
        Ok(())
    }

    #[tokio::test]
    async fn boot_recovery_preserves_known_drain_from_recovery_required() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("boot-recovery-drained-log");
        let log = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let service = ConversationInputService::new(Arc::clone(&log));
        service
            .submit(
                address(),
                "boot-recovery-drained".to_string(),
                "boot-recovery-drained".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let projection = service
            .dispatch_next(&address(), "boot-recovery-drained-turn".to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "boot recovery-drained attempt is missing".to_string())?;
        let attempt = exact_attempt(&projection)?;
        service
            .mailbox_accepted(attempt.clone())
            .await
            .map_err(|error| error.to_string())?;
        service
            .recovery_required_with_drain(
                attempt,
                "drain was observed before receipt projection failed".to_string(),
                true,
            )
            .await
            .map_err(|error| error.to_string())?;
        drop(service);

        assert_eq!(
            log.reconcile_conversation_inputs_at_boot()
                .map_err(|error| error.to_string())?,
            1
        );
        let reopened = ConversationInputService::new(log);
        let terminal = reopened
            .submit(
                address(),
                "boot-recovery-drained".to_string(),
                "boot-recovery-drained".to_string(),
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(terminal.phase, ConversationInputPhase::TurnSettled);
        assert!(terminal.drained);
        assert_eq!(terminal.outcome, Some(ConversationInputOutcome::Dropped));
        Ok(())
    }
}
