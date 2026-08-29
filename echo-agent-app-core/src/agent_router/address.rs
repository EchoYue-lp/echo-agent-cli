// Durable application-owned messaging between workspace conversations.
//
// The router persists accepted messages before any wake attempt. It does not
// write conversation transcripts and does not own an Agent executor; later
// delivery stages must invoke the existing chat driver for the target host.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use echo_agent::retry::RetryPolicy;
use echo_agent::state::journal::{
    CheckpointApplyStatus, CheckpointStore, CheckpointedApplyError, CheckpointedReducer,
    EventJournal, EventReducer, FileCheckpointStore, JournalBatchAppendError, JournalBatchLookup,
    JournalDurabilityStatus, PreparedJournalBatch, SegmentedFileEventJournal,
};
use echo_agent::utils::fs::FileDurability;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::workspace::WorkspaceId;

const MAX_MESSAGE_ID_CHARS: usize = 128;
const MAX_CONVERSATION_ID_CHARS: usize = 512;
const MAX_TEXT_CHARS: usize = 100_000;
const INBOX_SEGMENT_BYTES: u64 = 1024 * 1024;
const INBOX_CHECKPOINT_EVERY: u64 = 64;
const INBOX_MAX_SEGMENTS: usize = 8;
const INBOX_TERMINAL_RETENTION: usize = 256;
const INBOX_TERMINAL_RETENTION_BYTES: usize = 256 * 1024;
const MAX_INBOX_APPEND_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentAddress")]
pub struct AgentAddress {
    pub workspace_id: WorkspaceId,
    pub conversation_id: String,
}

impl AgentAddress {
    pub fn new(workspace_id: WorkspaceId, conversation_id: impl Into<String>) -> Self {
        Self {
            workspace_id,
            conversation_id: conversation_id.into(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), AgentRouterError> {
        let workspace_id = self.workspace_id.as_str();
        if workspace_id.trim().is_empty() {
            return Err(AgentRouterError::Validation(
                "workspace id must not be empty".to_string(),
            ));
        }
        if self.conversation_id.trim().is_empty() {
            return Err(AgentRouterError::Validation(
                "conversation id must not be empty".to_string(),
            ));
        }
        if self.conversation_id.chars().count() > MAX_CONVERSATION_ID_CHARS {
            return Err(AgentRouterError::Validation(format!(
                "conversation id exceeds {MAX_CONVERSATION_ID_CHARS} characters"
            )));
        }
        Ok(())
    }
}

/// One persistent group member. The role names a registered Subagent role in
/// the member workspace; it is intentionally dynamic rather than a closed
/// product enum.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentGroupMember")]
pub struct AgentGroupMember {
    pub address: AgentAddress,
    pub subagent_role: String,
    pub label: Option<String>,
}

/// Persistent cross-workspace address book. Execution remains owned by the
/// leader's existing TaskRun and framework DAG runtime.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, rename = "AgentGroup")]
pub struct AgentGroup {
    pub group_id: String,
    pub name: String,
    pub leader: AgentAddress,
    pub members: Vec<AgentGroupMember>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentGroup {
    fn validate(&self) -> Result<(), AgentRouterError> {
        if self.group_id.trim().is_empty() || self.group_id.chars().count() > MAX_MESSAGE_ID_CHARS {
            return Err(AgentRouterError::Validation(
                "Agent group id must contain 1-128 characters".to_string(),
            ));
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 160 {
            return Err(AgentRouterError::Validation(
                "Agent group name must contain 1-160 characters".to_string(),
            ));
        }
        self.leader.validate()?;
        if self.members.is_empty() {
            return Err(AgentRouterError::Validation(
                "Agent group requires at least one Subagent member".to_string(),
            ));
        }
        let mut addresses = HashSet::new();
        let mut roles = HashSet::new();
        for member in &self.members {
            member.address.validate()?;
            if member.address == self.leader {
                return Err(AgentRouterError::Validation(
                    "Agent group leader cannot also be a Subagent member".to_string(),
                ));
            }
            let role = member.subagent_role.trim();
            if role.is_empty() || role.chars().count() > 128 {
                return Err(AgentRouterError::Validation(
                    "Agent group member Subagent role must contain 1-128 characters".to_string(),
                ));
            }
            if member
                .label
                .as_deref()
                .is_some_and(|label| label.chars().count() > 160)
            {
                return Err(AgentRouterError::Validation(
                    "Agent group member label exceeds 160 characters".to_string(),
                ));
            }
            if !addresses.insert(member.address.clone()) {
                return Err(AgentRouterError::Validation(
                    "Agent group contains a duplicate member address".to_string(),
                ));
            }
            if !roles.insert(role.to_string()) {
                return Err(AgentRouterError::Validation(format!(
                    "Agent group contains duplicate Subagent role '{role}'"
                )));
            }
        }
        Ok(())
    }

    pub fn member_for_role(&self, role: &str) -> Option<&AgentGroupMember> {
        self.members
            .iter()
            .find(|member| member.subagent_role == role)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageOrigin {
    User,
    Agent,
    System,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentMessagePayload {
    Text { text: String },
    Reply { text: String },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub message_id: String,
    pub from: Option<AgentAddress>,
    pub to: AgentAddress,
    pub payload: AgentMessagePayload,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub origin: AgentMessageOrigin,
    pub created_at: DateTime<Utc>,
}

impl AgentMessage {
    pub fn user_text(
        from: Option<AgentAddress>,
        to: AgentAddress,
        text: impl Into<String>,
    ) -> Self {
        Self {
            message_id: uuid::Uuid::new_v4().to_string(),
            from,
            to,
            payload: AgentMessagePayload::Text { text: text.into() },
            correlation_id: None,
            causation_id: None,
            origin: AgentMessageOrigin::User,
            created_at: Utc::now(),
        }
    }

    /// Construct a model/runtime-authored text message. Unlike `user_text`,
    /// this origin is rendered as inter-agent guidance and cannot be treated
    /// as a direct user approval by the receiving Agent.
    pub fn agent_text(
        from: Option<AgentAddress>,
        to: AgentAddress,
        text: impl Into<String>,
    ) -> Self {
        Self {
            message_id: uuid::Uuid::new_v4().to_string(),
            from,
            to,
            payload: AgentMessagePayload::Text { text: text.into() },
            correlation_id: None,
            causation_id: None,
            origin: AgentMessageOrigin::Agent,
            created_at: Utc::now(),
        }
    }

    pub fn agent_reply(
        from: AgentAddress,
        to: AgentAddress,
        text: impl Into<String>,
        correlation_id: impl Into<String>,
        causation_id: impl Into<String>,
    ) -> Self {
        let causation_id = causation_id.into();
        let reply_identity = format!(
            "{}\0{}\0{}\0{}\0{causation_id}",
            from.workspace_id, from.conversation_id, to.workspace_id, to.conversation_id
        );
        Self {
            message_id: format!("agent-reply:{}", stable_segment(&reply_identity)),
            from: Some(from),
            to,
            payload: AgentMessagePayload::Reply { text: text.into() },
            correlation_id: Some(correlation_id.into()),
            causation_id: Some(causation_id),
            origin: AgentMessageOrigin::Agent,
            created_at: Utc::now(),
        }
    }

    /// Stable transcript identity for one cold delivery attempt across restarts.
    pub fn delivery_turn_id(&self) -> String {
        format!("agent-message:{}", stable_segment(&self.message_id))
    }

    fn validate(&self) -> Result<(), AgentRouterError> {
        if self.message_id.trim().is_empty() {
            return Err(AgentRouterError::Validation(
                "message id must not be empty".to_string(),
            ));
        }
        if self.message_id.chars().count() > MAX_MESSAGE_ID_CHARS {
            return Err(AgentRouterError::Validation(format!(
                "message id exceeds {MAX_MESSAGE_ID_CHARS} characters"
            )));
        }
        if let Some(from) = self.from.as_ref() {
            from.validate()?;
        }
        self.to.validate()?;
        let text = match &self.payload {
            AgentMessagePayload::Text { text } | AgentMessagePayload::Reply { text } => text,
        };
        if text.trim().is_empty() {
            return Err(AgentRouterError::Validation(
                "message text must not be empty".to_string(),
            ));
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(AgentRouterError::Validation(format!(
                "message text exceeds {MAX_TEXT_CHARS} characters"
            )));
        }
        Ok(())
    }

    pub fn text(&self) -> &str {
        match &self.payload {
            AgentMessagePayload::Text { text } | AgentMessagePayload::Reply { text } => text,
        }
    }

    pub fn expects_reply(&self) -> bool {
        matches!(&self.payload, AgentMessagePayload::Text { .. }) && self.from.is_some()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDeliveryPhase {
    Persisted,
    Claimed,
    MailboxAccepted,
    Drained,
    TurnSettled,
}

impl AgentDeliveryPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Persisted => "persisted",
            Self::Claimed => "claimed",
            Self::MailboxAccepted => "mailbox_accepted",
            Self::Drained => "drained",
            Self::TurnSettled => "turn_settled",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDeliveryOutcome {
    Completed,
    Failed,
    Cancelled,
    Dropped,
    OutcomeUnknown,
}

impl AgentDeliveryOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Dropped => "dropped",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AgentDeliveryDurability {
    Unconfirmed,
    Confirmed,
    Degraded { error: String },
}

impl From<JournalDurabilityStatus> for AgentDeliveryDurability {
    fn from(value: JournalDurabilityStatus) -> Self {
        match value {
            JournalDurabilityStatus::Unconfirmed => Self::Unconfirmed,
            JournalDurabilityStatus::Confirmed => Self::Confirmed,
            JournalDurabilityStatus::Degraded { error } => Self::Degraded { error },
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentDeliveryReceipt {
    pub message_id: String,
    pub target: AgentAddress,
    pub phase: AgentDeliveryPhase,
    pub outcome: Option<AgentDeliveryOutcome>,
    pub drained: bool,
    pub reason: Option<String>,
    pub persisted_at: DateTime<Utc>,
    pub duplicate: bool,
    /// Typed durability of the authoritative inbox commit. Degraded means the
    /// event owns its sequence and must not be retried.
    pub durability: AgentDeliveryDurability,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentDeliveryClaim {
    pub message: AgentMessage,
    pub attempt_id: String,
    pub attempt: u32,
    pub claimed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AgentDeliveryInFlight {
    pub claim: AgentDeliveryClaim,
    pub phase: AgentDeliveryPhase,
    pub effect_started: bool,
    pub turn_id: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentDeliveryRecord {
    pub message: AgentMessage,
    pub message_id: String,
    pub target: AgentAddress,
    pub phase: AgentDeliveryPhase,
    pub outcome: Option<AgentDeliveryOutcome>,
    pub drained: bool,
    pub reason: Option<String>,
    pub persisted_at: DateTime<Utc>,
    pub attempt_id: Option<String>,
    pub attempt: u32,
    pub claimed_at: Option<DateTime<Utc>>,
    pub mailbox_accepted_at: Option<DateTime<Utc>>,
    pub drained_at: Option<DateTime<Utc>>,
    pub turn_settled_at: Option<DateTime<Utc>>,
    pub turn_id: Option<String>,
    pub reply_message_id: Option<String>,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentEndpoint {
    pub address: AgentAddress,
    pub workspace_name: String,
    pub conversation_title: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum AgentInboxEvent {
    Persisted {
        message: AgentMessage,
        persisted_at: DateTime<Utc>,
    },
    Claimed {
        message_id: String,
        attempt_id: String,
        attempt: u32,
        claimed_at: DateTime<Utc>,
    },
    EffectStarted {
        message_id: String,
        attempt_id: String,
        started_at: DateTime<Utc>,
        turn_id: String,
    },
    MailboxAccepted {
        message_id: String,
        attempt_id: String,
        accepted_at: DateTime<Utc>,
        turn_id: String,
    },
    Drained {
        message_id: String,
        attempt_id: String,
        drained_at: DateTime<Utc>,
        turn_id: String,
    },
    Deferred {
        message_id: String,
        attempt_id: String,
        deferred_at: DateTime<Utc>,
        reason: String,
        #[serde(default)]
        next_attempt_at: Option<DateTime<Utc>>,
    },
    TurnSettled {
        message_id: String,
        attempt_id: String,
        settled_at: DateTime<Utc>,
        turn_id: Option<String>,
        outcome: AgentDeliveryOutcome,
        drained: bool,
        reason: Option<String>,
        retryable: bool,
        next_attempt_at: Option<DateTime<Utc>>,
        reply_message_id: Option<String>,
    },
}
