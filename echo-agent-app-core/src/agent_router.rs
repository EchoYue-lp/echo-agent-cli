//! Durable application-owned messaging between workspace conversations.
//!
//! The router persists accepted messages before any wake attempt. It does not
//! write conversation transcripts and does not own an Agent executor; later
//! delivery stages must invoke the existing chat driver for the target host.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use chrono::{DateTime, Utc};
use echo_core::retry::RetryPolicy;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use ts_rs::TS;

use crate::workspace::WorkspaceId;

const MAX_MESSAGE_ID_CHARS: usize = 128;
const MAX_CONVERSATION_ID_CHARS: usize = 512;
const MAX_TEXT_CHARS: usize = 100_000;

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
pub enum AgentDeliveryStatus {
    Queued,
    Claimed,
    Injected,
    Delivered,
    Failed,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentDeliveryReceipt {
    pub message_id: String,
    pub target: AgentAddress,
    pub status: AgentDeliveryStatus,
    pub accepted_at: DateTime<Utc>,
    pub duplicate: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentDeliveryClaim {
    pub message: AgentMessage,
    pub attempt_id: String,
    pub attempt: u32,
    pub claimed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentDeliveryRecord {
    pub message: AgentMessage,
    pub message_id: String,
    pub target: AgentAddress,
    pub status: AgentDeliveryStatus,
    pub accepted_at: DateTime<Utc>,
    pub attempt_id: Option<String>,
    pub attempt: u32,
    pub settled_at: Option<DateTime<Utc>>,
    pub turn_id: Option<String>,
    pub reply_message_id: Option<String>,
    pub error: Option<String>,
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
#[serde(tag = "event", rename_all = "snake_case")]
enum AgentInboxEvent {
    Accepted {
        message: AgentMessage,
        accepted_at: DateTime<Utc>,
    },
    Claimed {
        message_id: String,
        attempt_id: String,
        attempt: u32,
        claimed_at: DateTime<Utc>,
    },
    Injected {
        message_id: String,
        attempt_id: String,
        injected_at: DateTime<Utc>,
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
    Delivered {
        message_id: String,
        attempt_id: String,
        delivered_at: DateTime<Utc>,
        turn_id: String,
        reply_message_id: Option<String>,
    },
    Failed {
        message_id: String,
        attempt_id: String,
        failed_at: DateTime<Utc>,
        error: String,
        retryable: bool,
        #[serde(default)]
        next_attempt_at: Option<DateTime<Utc>>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum AgentRouterError {
    #[error("invalid Agent message: {0}")]
    Validation(String),
    #[error("Agent message id '{message_id}' already identifies different content")]
    IdCollision { message_id: String },
    #[error("Agent router I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Agent router data is corrupt at {path}: {message}")]
    Corrupt { path: PathBuf, message: String },
    #[error("Agent router task failed: {0}")]
    Task(String),
    #[error("Agent delivery claim '{attempt_id}' is stale for message '{message_id}'")]
    StaleClaim {
        message_id: String,
        attempt_id: String,
    },
    #[error("Agent delivery supervisor is shutting down")]
    ShuttingDown,
    #[error("Agent delivery supervisor requires an active Tokio runtime: {0}")]
    RuntimeUnavailable(String),
    #[error("Agent delivery supervisor state is unavailable")]
    StateUnavailable,
    #[error("Agent group '{0}' does not exist")]
    GroupNotFound(String),
}

/// File-backed durable inbox owner.
pub struct AgentRouter {
    root: PathBuf,
    mutation: Mutex<()>,
}

#[derive(Default)]
struct AgentDeliverySupervisorState {
    active: HashSet<AgentAddress>,
    dirty: HashSet<AgentAddress>,
    drivers: tokio::task::JoinSet<AgentAddress>,
    shutting_down: bool,
}

/// Application-owned lifetime manager for asynchronous inbox delivery.
/// It owns task lifetimes only; Agent execution remains in `drive_chat`.
pub struct AgentDeliverySupervisor {
    state: StdMutex<AgentDeliverySupervisorState>,
    cancel: echo_agent::agent::CancellationToken,
}

impl Default for AgentDeliverySupervisor {
    fn default() -> Self {
        Self {
            state: StdMutex::new(AgentDeliverySupervisorState::default()),
            cancel: echo_agent::agent::CancellationToken::new(),
        }
    }
}

impl AgentDeliverySupervisor {
    pub fn cancellation_token(&self) -> echo_agent::agent::CancellationToken {
        self.cancel.clone()
    }

    pub fn has_active_workspace(&self, workspace_id: &WorkspaceId) -> bool {
        self.state
            .lock()
            .map(|state| {
                state
                    .active
                    .iter()
                    .any(|target| &target.workspace_id == workspace_id)
            })
            .unwrap_or(true)
    }

    /// Start one target-owned delivery task or mark the already-running task
    /// dirty so it performs another empty-inbox check before exit.
    pub fn supervise<F>(&self, target: AgentAddress, operation: F) -> Result<bool, AgentRouterError>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| AgentRouterError::RuntimeUnavailable(error.to_string()))?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        Self::collect_finished(&mut state);
        if state.shutting_down {
            return Err(AgentRouterError::ShuttingDown);
        }
        if state.active.contains(&target) {
            state.dirty.insert(target);
            return Ok(false);
        }
        state.active.insert(target.clone());
        state.drivers.spawn_on(
            async move {
                operation.await;
                target
            },
            &runtime,
        );
        Ok(true)
    }

    /// Complete one drain cycle. `true` means an enqueue raced the cycle and
    /// the same owned task must inspect the target again before exiting.
    pub fn complete_cycle(&self, target: &AgentAddress) -> Result<bool, AgentRouterError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        if state.dirty.remove(target) && !state.shutting_down {
            return Ok(true);
        }
        state.active.remove(target);
        Ok(false)
    }

    fn collect_finished(state: &mut AgentDeliverySupervisorState) {
        while let Some(result) = state.drivers.try_join_next() {
            match result {
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(%error, "Agent delivery task failed to join");
                }
            }
        }
    }

    pub async fn shutdown(&self) -> Result<(), AgentRouterError> {
        self.cancel.cancel();
        let mut drivers = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            state.shutting_down = true;
            state.active.clear();
            state.dirty.clear();
            std::mem::take(&mut state.drivers)
        };
        let mut failures = Vec::new();
        while let Some(result) = drivers.join_next().await {
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AgentRouterError::Task(failures.join("; ")))
        }
    }
}

impl AgentRouter {
    pub fn at_default_root() -> Arc<Self> {
        Arc::new(Self::new(echo_agent::paths::user_data_path("agent-router")))
    }

    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            mutation: Mutex::new(()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn list_groups(&self) -> Result<Vec<AgentGroup>, AgentRouterError> {
        let _mutation = self.mutation.lock().await;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || list_groups_sync(&root))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn create_group(
        &self,
        name: impl Into<String>,
        leader: AgentAddress,
        members: Vec<AgentGroupMember>,
    ) -> Result<AgentGroup, AgentRouterError> {
        let now = Utc::now();
        let group = AgentGroup {
            group_id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            leader,
            members,
            created_at: now,
            updated_at: now,
        };
        group.validate()?;
        let _mutation = self.mutation.lock().await;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || create_group_sync(&root, group))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn update_group(
        &self,
        group_id: impl Into<String>,
        name: impl Into<String>,
        leader: AgentAddress,
        members: Vec<AgentGroupMember>,
    ) -> Result<AgentGroup, AgentRouterError> {
        let group_id = group_id.into();
        let name = name.into();
        let _mutation = self.mutation.lock().await;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            update_group_sync(&root, &group_id, name, leader, members)
        })
        .await
        .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn delete_group(&self, group_id: &str) -> Result<bool, AgentRouterError> {
        if group_id.trim().is_empty() {
            return Err(AgentRouterError::Validation(
                "Agent group id must not be empty".to_string(),
            ));
        }
        let _mutation = self.mutation.lock().await;
        let root = self.root.clone();
        let group_id = group_id.to_string();
        tokio::task::spawn_blocking(move || delete_group_sync(&root, &group_id))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    /// Persist a message exactly once by `message_id` within its target inbox.
    /// Repeating the same message returns the original acceptance receipt.
    pub async fn enqueue(
        &self,
        message: AgentMessage,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        message.validate()?;
        let _mutation = self.mutation.lock().await;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || enqueue_sync(&root, message))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn pending(
        &self,
        target: &AgentAddress,
    ) -> Result<Vec<AgentMessage>, AgentRouterError> {
        target.validate()?;
        let _mutation = self.mutation.lock().await;
        let root = self.root.clone();
        let target = target.clone();
        tokio::task::spawn_blocking(move || pending_sync(&root, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn claim_next(
        &self,
        target: &AgentAddress,
    ) -> Result<Option<AgentDeliveryClaim>, AgentRouterError> {
        target.validate()?;
        let _mutation = self.mutation.lock().await;
        let root = self.root.clone();
        let target = target.clone();
        tokio::task::spawn_blocking(move || claim_next_sync(&root, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn next_attempt_at(
        &self,
        target: &AgentAddress,
    ) -> Result<Option<DateTime<Utc>>, AgentRouterError> {
        target.validate()?;
        let _mutation = self.mutation.lock().await;
        let root = self.root.clone();
        let target = target.clone();
        tokio::task::spawn_blocking(move || next_attempt_at_sync(&root, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn defer(
        &self,
        claim: &AgentDeliveryClaim,
        reason: impl Into<String>,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        let next_attempt_at = retry_deadline(claim.attempt);
        self.settle_claim(
            claim,
            ClaimSettlement::Deferred {
                reason: reason.into(),
                next_attempt_at,
            },
        )
        .await
    }

    pub async fn injected(
        &self,
        claim: &AgentDeliveryClaim,
        turn_id: impl Into<String>,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        self.settle_claim(
            claim,
            ClaimSettlement::Injected {
                turn_id: turn_id.into(),
            },
        )
        .await
    }

    pub async fn delivered(
        &self,
        claim: &AgentDeliveryClaim,
        turn_id: impl Into<String>,
        reply_message_id: Option<String>,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        self.settle_claim(
            claim,
            ClaimSettlement::Delivered {
                turn_id: turn_id.into(),
                reply_message_id,
            },
        )
        .await
    }

    pub async fn failed(
        &self,
        claim: &AgentDeliveryClaim,
        error: impl Into<String>,
        retryable: bool,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        let next_attempt_at = retryable.then(|| retry_deadline(claim.attempt));
        self.settle_claim(
            claim,
            ClaimSettlement::Failed {
                error: error.into(),
                retryable,
                next_attempt_at,
            },
        )
        .await
    }

    pub async fn records(
        &self,
        target: &AgentAddress,
    ) -> Result<Vec<AgentDeliveryRecord>, AgentRouterError> {
        target.validate()?;
        let _mutation = self.mutation.lock().await;
        let root = self.root.clone();
        let target = target.clone();
        tokio::task::spawn_blocking(move || records_sync(&root, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    async fn settle_claim(
        &self,
        claim: &AgentDeliveryClaim,
        settlement: ClaimSettlement,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        let _mutation = self.mutation.lock().await;
        let root = self.root.clone();
        let claim = claim.clone();
        tokio::task::spawn_blocking(move || settle_claim_sync(&root, &claim, settlement))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }
}

enum ClaimSettlement {
    Injected {
        turn_id: String,
    },
    Deferred {
        reason: String,
        next_attempt_at: DateTime<Utc>,
    },
    Delivered {
        turn_id: String,
        reply_message_id: Option<String>,
    },
    Failed {
        error: String,
        retryable: bool,
        next_attempt_at: Option<DateTime<Utc>>,
    },
}

fn enqueue_sync(
    root: &Path,
    message: AgentMessage,
) -> Result<AgentDeliveryReceipt, AgentRouterError> {
    let target = message.to.clone();
    with_inbox_lock(root, &target, |events_path| {
        let mut events = read_events(events_path)?;
        let folded = fold_events(events_path, &events)?;
        if let Some(existing) = folded
            .iter()
            .find(|entry| entry.message.message_id == message.message_id)
        {
            if !same_logical_message(&existing.message, &message) {
                return Err(AgentRouterError::IdCollision {
                    message_id: message.message_id.clone(),
                });
            }
            return Ok(AgentDeliveryReceipt {
                message_id: existing.message.message_id.clone(),
                target: existing.message.to.clone(),
                status: existing.status,
                accepted_at: existing.accepted_at,
                duplicate: true,
            });
        }

        let accepted_at = Utc::now();
        events.push(AgentInboxEvent::Accepted {
            message: message.clone(),
            accepted_at,
        });
        write_events(events_path, &events)?;
        Ok(AgentDeliveryReceipt {
            message_id: message.message_id,
            target: message.to,
            status: AgentDeliveryStatus::Queued,
            accepted_at,
            duplicate: false,
        })
    })
}

fn same_logical_message(left: &AgentMessage, right: &AgentMessage) -> bool {
    left.message_id == right.message_id
        && left.from == right.from
        && left.to == right.to
        && left.payload == right.payload
        && left.correlation_id == right.correlation_id
        && left.causation_id == right.causation_id
        && left.origin == right.origin
}

fn pending_sync(root: &Path, target: &AgentAddress) -> Result<Vec<AgentMessage>, AgentRouterError> {
    with_inbox_lock(root, target, |events_path| {
        let events = read_events(events_path)?;
        Ok(fold_events(events_path, &events)?
            .into_iter()
            .filter(|entry| !entry.terminal)
            .map(|entry| entry.message)
            .collect())
    })
}

fn claim_next_sync(
    root: &Path,
    target: &AgentAddress,
) -> Result<Option<AgentDeliveryClaim>, AgentRouterError> {
    with_inbox_lock(root, target, |events_path| {
        let mut events = read_events(events_path)?;
        let folded = fold_events(events_path, &events)?;
        let Some(next) = folded.into_iter().find(|entry| !entry.terminal) else {
            return Ok(None);
        };
        if next
            .next_attempt_at
            .is_some_and(|deadline| deadline > Utc::now())
        {
            return Ok(None);
        }
        let attempt = next.attempt.saturating_add(1);
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let claimed_at = Utc::now();
        events.push(AgentInboxEvent::Claimed {
            message_id: next.message.message_id.clone(),
            attempt_id: attempt_id.clone(),
            attempt,
            claimed_at,
        });
        write_events(events_path, &events)?;
        Ok(Some(AgentDeliveryClaim {
            message: next.message,
            attempt_id,
            attempt,
            claimed_at,
        }))
    })
}

fn settle_claim_sync(
    root: &Path,
    claim: &AgentDeliveryClaim,
    settlement: ClaimSettlement,
) -> Result<AgentDeliveryReceipt, AgentRouterError> {
    let target = claim.message.to.clone();
    with_inbox_lock(root, &target, |events_path| {
        let mut events = read_events(events_path)?;
        let folded = fold_events(events_path, &events)?;
        let entry = folded
            .iter()
            .find(|entry| entry.message.message_id == claim.message.message_id)
            .ok_or_else(|| AgentRouterError::StaleClaim {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
            })?;
        if entry.attempt_id.as_deref() != Some(claim.attempt_id.as_str())
            || !matches!(
                entry.status,
                AgentDeliveryStatus::Claimed | AgentDeliveryStatus::Injected
            )
        {
            return Err(AgentRouterError::StaleClaim {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
            });
        }
        let status = match settlement {
            ClaimSettlement::Injected { turn_id } => {
                events.push(AgentInboxEvent::Injected {
                    message_id: claim.message.message_id.clone(),
                    attempt_id: claim.attempt_id.clone(),
                    injected_at: Utc::now(),
                    turn_id,
                });
                AgentDeliveryStatus::Injected
            }
            ClaimSettlement::Deferred {
                reason,
                next_attempt_at,
            } => {
                events.push(AgentInboxEvent::Deferred {
                    message_id: claim.message.message_id.clone(),
                    attempt_id: claim.attempt_id.clone(),
                    deferred_at: Utc::now(),
                    reason,
                    next_attempt_at: Some(next_attempt_at),
                });
                AgentDeliveryStatus::Queued
            }
            ClaimSettlement::Delivered {
                turn_id,
                reply_message_id,
            } => {
                events.push(AgentInboxEvent::Delivered {
                    message_id: claim.message.message_id.clone(),
                    attempt_id: claim.attempt_id.clone(),
                    delivered_at: Utc::now(),
                    turn_id,
                    reply_message_id,
                });
                AgentDeliveryStatus::Delivered
            }
            ClaimSettlement::Failed {
                error,
                retryable,
                next_attempt_at,
            } => {
                events.push(AgentInboxEvent::Failed {
                    message_id: claim.message.message_id.clone(),
                    attempt_id: claim.attempt_id.clone(),
                    failed_at: Utc::now(),
                    error,
                    retryable,
                    next_attempt_at,
                });
                AgentDeliveryStatus::Failed
            }
        };
        let accepted_at = entry.accepted_at;
        write_events(events_path, &events)?;
        Ok(AgentDeliveryReceipt {
            message_id: claim.message.message_id.clone(),
            target: target.clone(),
            status,
            accepted_at,
            duplicate: false,
        })
    })
}

fn records_sync(
    root: &Path,
    target: &AgentAddress,
) -> Result<Vec<AgentDeliveryRecord>, AgentRouterError> {
    with_inbox_lock(root, target, |events_path| {
        let events = read_events(events_path)?;
        Ok(fold_events(events_path, &events)?
            .into_iter()
            .map(FoldedDelivery::record)
            .collect())
    })
}

fn next_attempt_at_sync(
    root: &Path,
    target: &AgentAddress,
) -> Result<Option<DateTime<Utc>>, AgentRouterError> {
    with_inbox_lock(root, target, |events_path| {
        let events = read_events(events_path)?;
        Ok(fold_events(events_path, &events)?
            .into_iter()
            .find(|entry| !entry.terminal)
            .and_then(|entry| entry.next_attempt_at))
    })
}

fn retry_deadline(attempt: u32) -> DateTime<Utc> {
    let delay = RetryPolicy::default()
        .delay_for(attempt.max(1))
        .max(std::time::Duration::from_millis(100));
    let chrono_delay =
        chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::seconds(30));
    Utc::now() + chrono_delay
}

fn list_groups_sync(root: &Path) -> Result<Vec<AgentGroup>, AgentRouterError> {
    with_groups_lock(root, |groups_path| {
        let mut groups = read_groups(groups_path)?;
        groups.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.group_id.cmp(&right.group_id))
        });
        Ok(groups)
    })
}

fn create_group_sync(root: &Path, group: AgentGroup) -> Result<AgentGroup, AgentRouterError> {
    with_groups_lock(root, |groups_path| {
        let mut groups = read_groups(groups_path)?;
        if groups
            .iter()
            .any(|existing| existing.group_id == group.group_id)
        {
            return Err(AgentRouterError::IdCollision {
                message_id: group.group_id.clone(),
            });
        }
        groups.push(group.clone());
        write_groups(groups_path, &groups)?;
        Ok(group)
    })
}

fn update_group_sync(
    root: &Path,
    group_id: &str,
    name: String,
    leader: AgentAddress,
    members: Vec<AgentGroupMember>,
) -> Result<AgentGroup, AgentRouterError> {
    with_groups_lock(root, |groups_path| {
        let mut groups = read_groups(groups_path)?;
        let existing = groups
            .iter_mut()
            .find(|group| group.group_id == group_id)
            .ok_or_else(|| AgentRouterError::GroupNotFound(group_id.to_string()))?;
        let updated = AgentGroup {
            group_id: existing.group_id.clone(),
            name,
            leader,
            members,
            created_at: existing.created_at,
            updated_at: Utc::now(),
        };
        updated.validate()?;
        *existing = updated.clone();
        write_groups(groups_path, &groups)?;
        Ok(updated)
    })
}

fn delete_group_sync(root: &Path, group_id: &str) -> Result<bool, AgentRouterError> {
    with_groups_lock(root, |groups_path| {
        let mut groups = read_groups(groups_path)?;
        let before = groups.len();
        groups.retain(|group| group.group_id != group_id);
        if groups.len() == before {
            return Ok(false);
        }
        write_groups(groups_path, &groups)?;
        Ok(true)
    })
}

fn with_groups_lock<T>(
    root: &Path,
    operation: impl FnOnce(&Path) -> Result<T, AgentRouterError>,
) -> Result<T, AgentRouterError> {
    std::fs::create_dir_all(root).map_err(|source| AgentRouterError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let lock_path = root.join("groups.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| AgentRouterError::Io {
            path: lock_path.clone(),
            source,
        })?;
    lock.lock_exclusive()
        .map_err(|source| AgentRouterError::Io {
            path: lock_path.clone(),
            source,
        })?;
    let result = operation(&root.join("groups.json"));
    let unlock = FileExt::unlock(&lock).map_err(|source| AgentRouterError::Io {
        path: lock_path,
        source,
    });
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn read_groups(path: &Path) -> Result<Vec<AgentGroup>, AgentRouterError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AgentRouterError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let groups: Vec<AgentGroup> =
        serde_json::from_slice(&bytes).map_err(|error| AgentRouterError::Corrupt {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    for group in &groups {
        group.validate()?;
    }
    Ok(groups)
}

fn write_groups(path: &Path, groups: &[AgentGroup]) -> Result<(), AgentRouterError> {
    let encoded = serde_json::to_vec_pretty(groups).map_err(|error| AgentRouterError::Corrupt {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    echo_core::utils::fs::atomic_write(path, &encoded).map_err(|source| AgentRouterError::Io {
        path: path.to_path_buf(),
        source,
    })
}

struct FoldedDelivery {
    message: AgentMessage,
    accepted_at: DateTime<Utc>,
    status: AgentDeliveryStatus,
    attempt_id: Option<String>,
    attempt: u32,
    settled_at: Option<DateTime<Utc>>,
    turn_id: Option<String>,
    reply_message_id: Option<String>,
    error: Option<String>,
    next_attempt_at: Option<DateTime<Utc>>,
    terminal: bool,
}

impl FoldedDelivery {
    fn record(self) -> AgentDeliveryRecord {
        let message = self.message;
        AgentDeliveryRecord {
            message_id: message.message_id.clone(),
            target: message.to.clone(),
            message,
            status: self.status,
            accepted_at: self.accepted_at,
            attempt_id: self.attempt_id,
            attempt: self.attempt,
            settled_at: self.settled_at,
            turn_id: self.turn_id,
            reply_message_id: self.reply_message_id,
            error: self.error,
            next_attempt_at: self.next_attempt_at,
        }
    }
}

fn fold_events(
    path: &Path,
    events: &[AgentInboxEvent],
) -> Result<Vec<FoldedDelivery>, AgentRouterError> {
    let mut order = Vec::new();
    let mut entries = HashMap::<String, FoldedDelivery>::new();
    for event in events {
        match event {
            AgentInboxEvent::Accepted {
                message,
                accepted_at,
            } => {
                if entries.contains_key(&message.message_id) {
                    return Err(corrupt_event(
                        path,
                        format!("duplicate acceptance for {}", message.message_id),
                    ));
                }
                order.push(message.message_id.clone());
                entries.insert(
                    message.message_id.clone(),
                    FoldedDelivery {
                        message: message.clone(),
                        accepted_at: *accepted_at,
                        status: AgentDeliveryStatus::Queued,
                        attempt_id: None,
                        attempt: 0,
                        settled_at: None,
                        turn_id: None,
                        reply_message_id: None,
                        error: None,
                        next_attempt_at: None,
                        terminal: false,
                    },
                );
            }
            AgentInboxEvent::Claimed {
                message_id,
                attempt_id,
                attempt,
                claimed_at: _,
            } => {
                let entry = delivery_entry_mut(path, &mut entries, message_id)?;
                if entry.terminal {
                    return Err(corrupt_event(
                        path,
                        format!("terminal message {message_id} was claimed again"),
                    ));
                }
                entry.status = AgentDeliveryStatus::Claimed;
                entry.attempt_id = Some(attempt_id.clone());
                entry.attempt = *attempt;
                entry.settled_at = None;
                entry.error = None;
                entry.next_attempt_at = None;
            }
            AgentInboxEvent::Injected {
                message_id,
                attempt_id,
                injected_at,
                turn_id,
            } => {
                let entry = claimed_entry_mut(path, &mut entries, message_id, attempt_id)?;
                entry.status = AgentDeliveryStatus::Injected;
                entry.settled_at = Some(*injected_at);
                entry.turn_id = Some(turn_id.clone());
            }
            AgentInboxEvent::Deferred {
                message_id,
                attempt_id,
                deferred_at,
                reason: _,
                next_attempt_at,
            } => {
                let entry = claimed_entry_mut(path, &mut entries, message_id, attempt_id)?;
                entry.status = AgentDeliveryStatus::Queued;
                entry.settled_at = Some(*deferred_at);
                entry.next_attempt_at = *next_attempt_at;
            }
            AgentInboxEvent::Delivered {
                message_id,
                attempt_id,
                delivered_at,
                turn_id,
                reply_message_id,
            } => {
                let entry = claimed_entry_mut(path, &mut entries, message_id, attempt_id)?;
                entry.status = AgentDeliveryStatus::Delivered;
                entry.settled_at = Some(*delivered_at);
                entry.turn_id = Some(turn_id.clone());
                entry.reply_message_id = reply_message_id.clone();
                entry.terminal = true;
                entry.next_attempt_at = None;
            }
            AgentInboxEvent::Failed {
                message_id,
                attempt_id,
                failed_at,
                error,
                retryable,
                next_attempt_at,
            } => {
                let entry = claimed_entry_mut(path, &mut entries, message_id, attempt_id)?;
                entry.status = AgentDeliveryStatus::Failed;
                entry.settled_at = Some(*failed_at);
                entry.error = Some(error.clone());
                entry.terminal = !retryable;
                entry.next_attempt_at = *next_attempt_at;
            }
        }
    }
    order
        .into_iter()
        .map(|message_id| {
            entries.remove(&message_id).ok_or_else(|| {
                corrupt_event(
                    path,
                    format!("message {message_id} disappeared during fold"),
                )
            })
        })
        .collect()
}

fn delivery_entry_mut<'a>(
    path: &Path,
    entries: &'a mut HashMap<String, FoldedDelivery>,
    message_id: &str,
) -> Result<&'a mut FoldedDelivery, AgentRouterError> {
    entries.get_mut(message_id).ok_or_else(|| {
        corrupt_event(
            path,
            format!("delivery event references unknown message {message_id}"),
        )
    })
}

fn claimed_entry_mut<'a>(
    path: &Path,
    entries: &'a mut HashMap<String, FoldedDelivery>,
    message_id: &str,
    attempt_id: &str,
) -> Result<&'a mut FoldedDelivery, AgentRouterError> {
    let entry = delivery_entry_mut(path, entries, message_id)?;
    if !matches!(
        entry.status,
        AgentDeliveryStatus::Claimed | AgentDeliveryStatus::Injected
    ) || entry.attempt_id.as_deref() != Some(attempt_id)
    {
        return Err(corrupt_event(
            path,
            format!("delivery event has stale claim {attempt_id} for {message_id}"),
        ));
    }
    Ok(entry)
}

fn corrupt_event(path: &Path, message: String) -> AgentRouterError {
    AgentRouterError::Corrupt {
        path: path.to_path_buf(),
        message,
    }
}

fn with_inbox_lock<T>(
    root: &Path,
    target: &AgentAddress,
    operation: impl FnOnce(&Path) -> Result<T, AgentRouterError>,
) -> Result<T, AgentRouterError> {
    let inbox = inbox_dir(root, target);
    std::fs::create_dir_all(&inbox).map_err(|source| AgentRouterError::Io {
        path: inbox.clone(),
        source,
    })?;
    let lock_path = inbox.join("events.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| AgentRouterError::Io {
            path: lock_path.clone(),
            source,
        })?;
    lock.lock_exclusive()
        .map_err(|source| AgentRouterError::Io {
            path: lock_path.clone(),
            source,
        })?;
    let result = operation(&inbox.join("events.jsonl"));
    let unlock = FileExt::unlock(&lock).map_err(|source| AgentRouterError::Io {
        path: lock_path,
        source,
    });
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn inbox_dir(root: &Path, target: &AgentAddress) -> PathBuf {
    root.join("inboxes")
        .join(stable_segment(target.workspace_id.as_str()))
        .join(stable_segment(&target.conversation_id))
}

fn stable_segment(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn read_events(path: &Path) -> Result<Vec<AgentInboxEvent>, AgentRouterError> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AgentRouterError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(line_number, line)| {
            serde_json::from_str(line).map_err(|error| AgentRouterError::Corrupt {
                path: path.to_path_buf(),
                message: format!("line {}: {error}", line_number.saturating_add(1)),
            })
        })
        .collect()
}

fn write_events(path: &Path, events: &[AgentInboxEvent]) -> Result<(), AgentRouterError> {
    let mut encoded = Vec::new();
    for event in events {
        serde_json::to_writer(&mut encoded, event).map_err(|error| AgentRouterError::Corrupt {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        encoded.push(b'\n');
    }
    echo_core::utils::fs::atomic_write(path, &encoded).map_err(|source| AgentRouterError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address() -> AgentAddress {
        AgentAddress::new(WorkspaceId::from_name("workspace-b"), "conversation-b")
    }

    fn group_member(role: &str) -> AgentGroupMember {
        AgentGroupMember {
            address: address(),
            subagent_role: role.to_string(),
            label: Some("Remote specialist".to_string()),
        }
    }

    async fn drain_inbox(root: PathBuf, target: AgentAddress) -> Result<usize, String> {
        let router = AgentRouter::new(root);
        let mut delivered = 0usize;
        while let Some(claim) = router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
        {
            router
                .delivered(&claim, claim.message.delivery_turn_id(), None)
                .await
                .map_err(|error| error.to_string())?;
            delivered = delivered.saturating_add(1);
        }
        Ok(delivered)
    }

    #[tokio::test]
    async fn groups_persist_update_and_delete_without_runtime_state() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let leader = AgentAddress::new(WorkspaceId::from_name("workspace-a"), "conversation-a");
        let created = router
            .create_group(
                "Product team",
                leader.clone(),
                vec![group_member("explorer")],
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(created.member_for_role("explorer"), created.members.first());
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        assert_eq!(
            restarted
                .list_groups()
                .await
                .map_err(|error| error.to_string())?,
            vec![created.clone()]
        );
        let updated = restarted
            .update_group(
                created.group_id.clone(),
                "Product delivery",
                leader,
                vec![group_member("reviewer")],
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(updated.created_at, created.created_at);
        assert!(updated.updated_at >= created.updated_at);
        assert!(updated.member_for_role("reviewer").is_some());
        assert!(
            restarted
                .delete_group(&created.group_id)
                .await
                .map_err(|error| error.to_string())?
        );
        assert!(
            restarted
                .list_groups()
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn groups_reject_duplicate_roles_addresses_and_leader_membership() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let leader = AgentAddress::new(WorkspaceId::from_name("workspace-a"), "conversation-a");
        let duplicate_address = vec![group_member("explorer"), group_member("reviewer")];
        assert!(matches!(
            router
                .create_group("Duplicate address", leader.clone(), duplicate_address)
                .await,
            Err(AgentRouterError::Validation(_))
        ));

        let mut duplicate_role = group_member("explorer");
        duplicate_role.address =
            AgentAddress::new(WorkspaceId::from_name("workspace-c"), "conversation-c");
        assert!(matches!(
            router
                .create_group(
                    "Duplicate role",
                    leader.clone(),
                    vec![group_member("explorer"), duplicate_role],
                )
                .await,
            Err(AgentRouterError::Validation(_))
        ));

        assert!(matches!(
            router
                .create_group(
                    "Leader member",
                    leader.clone(),
                    vec![AgentGroupMember {
                        address: leader,
                        subagent_role: "explorer".to_string(),
                        label: None,
                    }],
                )
                .await,
            Err(AgentRouterError::Validation(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn accepted_message_survives_restart_and_duplicate_retry() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut message = AgentMessage::user_text(None, address(), "question");
        message.message_id = "stable-message".to_string();

        let first = router
            .enqueue(message.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(!first.duplicate);
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        let duplicate = restarted
            .enqueue(message.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.accepted_at, first.accepted_at);
        let mut later_retry = message.clone();
        later_retry.created_at += chrono::Duration::seconds(30);
        let later_duplicate = restarted
            .enqueue(later_retry)
            .await
            .map_err(|error| error.to_string())?;
        assert!(later_duplicate.duplicate);
        assert_eq!(
            restarted
                .pending(&address())
                .await
                .map_err(|e| e.to_string())?,
            vec![message]
        );
        Ok(())
    }

    #[tokio::test]
    async fn reply_identity_is_stable_for_one_causal_message() -> Result<(), String> {
        let source = AgentAddress::new(WorkspaceId::from_name("source"), "source-conversation");
        let target = address();
        let first = AgentMessage::agent_reply(
            target.clone(),
            source.clone(),
            "answer",
            "correlation",
            "causal-message",
        );
        let second = AgentMessage::agent_reply(
            target.clone(),
            source,
            "answer",
            "correlation",
            "causal-message",
        );
        assert_eq!(first.message_id, second.message_id);
        assert_eq!(first.delivery_turn_id(), second.delivery_turn_id());
        let other_target =
            AgentAddress::new(WorkspaceId::from_name("other-target"), "other-conversation");
        let other_reply = AgentMessage::agent_reply(
            other_target,
            target,
            "answer",
            "correlation",
            "causal-message",
        );
        assert_ne!(first.message_id, other_reply.message_id);

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        router
            .enqueue(first)
            .await
            .map_err(|error| error.to_string())?;
        let duplicate = router
            .enqueue(second)
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        Ok(())
    }

    #[tokio::test]
    async fn same_id_with_different_content_fails_closed() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut first = AgentMessage::user_text(None, address(), "first");
        first.message_id = "collision".to_string();
        router
            .enqueue(first.clone())
            .await
            .map_err(|error| error.to_string())?;
        let mut second = first;
        second.payload = AgentMessagePayload::Text {
            text: "second".to_string(),
        };

        assert!(matches!(
            router.enqueue(second).await,
            Err(AgentRouterError::IdCollision { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn corrupt_inbox_is_never_silently_replaced() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let events = inbox_dir(temp.path(), &target).join("events.jsonl");
        let parent = events
            .parent()
            .ok_or_else(|| "events parent missing".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::write(&events, "{broken\n").map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());

        assert!(matches!(
            router.pending(&target).await,
            Err(AgentRouterError::Corrupt { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(events).map_err(|error| error.to_string())?,
            "{broken\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn claims_are_fifo_deferred_and_terminally_settled() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut first = AgentMessage::user_text(None, address(), "first");
        first.message_id = "first".to_string();
        let mut second = AgentMessage::user_text(None, address(), "second");
        second.message_id = "second".to_string();
        router
            .enqueue(first.clone())
            .await
            .map_err(|error| error.to_string())?;
        router
            .enqueue(second.clone())
            .await
            .map_err(|error| error.to_string())?;

        let first_claim = router
            .claim_next(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "first claim missing".to_string())?;
        assert_eq!(first_claim.message, first);
        router
            .defer(&first_claim, "busy")
            .await
            .map_err(|error| error.to_string())?;
        let deadline = router
            .next_attempt_at(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "deferred claim lost its retry deadline".to_string())?;
        let delay = deadline
            .signed_duration_since(Utc::now())
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        if !delay.is_zero() {
            tokio::time::sleep(delay.saturating_add(std::time::Duration::from_millis(5))).await;
        }
        let retry = router
            .claim_next(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "deferred claim missing".to_string())?;
        assert_eq!(retry.message.message_id, "first");
        assert_eq!(retry.attempt, 2);
        router
            .delivered(&retry, "turn-first", None)
            .await
            .map_err(|error| error.to_string())?;
        let second_claim = router
            .claim_next(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "second claim missing".to_string())?;
        assert_eq!(second_claim.message, second);
        router
            .failed(&second_claim, "permanent", false)
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            router
                .pending(&address())
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        let records = router
            .records(&address())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(records.len(), 2);
        assert_eq!(
            records.first().map(|record| record.status),
            Some(AgentDeliveryStatus::Delivered)
        );
        assert_eq!(
            records.get(1).map(|record| record.status),
            Some(AgentDeliveryStatus::Failed)
        );
        Ok(())
    }

    #[tokio::test]
    async fn restart_reclaims_incomplete_attempt_and_rejects_stale_settlement() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut message = AgentMessage::user_text(None, address(), "recover");
        message.message_id = "recover".to_string();
        router
            .enqueue(message)
            .await
            .map_err(|error| error.to_string())?;
        let abandoned = router
            .claim_next(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "abandoned claim missing".to_string())?;
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        let recovered = restarted
            .claim_next(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "recovered claim missing".to_string())?;
        assert_eq!(recovered.attempt, 2);
        assert!(matches!(
            restarted.delivered(&abandoned, "stale", None).await,
            Err(AgentRouterError::StaleClaim { .. })
        ));
        restarted
            .delivered(&recovered, "recovered", None)
            .await
            .map_err(|error| error.to_string())?;
        let duplicate = restarted
            .enqueue(recovered.message)
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.status, AgentDeliveryStatus::Delivered);
        Ok(())
    }

    #[tokio::test]
    async fn three_workspace_inboxes_survive_restart_and_concurrent_drain() -> Result<(), String> {
        const MESSAGES_PER_WORKSPACE: usize = 32;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().to_path_buf();
        let targets = ["alpha", "beta", "gamma"]
            .into_iter()
            .map(|name| {
                AgentAddress::new(WorkspaceId::from_name(name), format!("{name}-conversation"))
            })
            .collect::<Vec<_>>();
        let router = AgentRouter::new(root.clone());
        let mut messages = Vec::new();
        for target in &targets {
            for offset in 0..MESSAGES_PER_WORKSPACE {
                let mut message =
                    AgentMessage::user_text(None, target.clone(), format!("message {offset}"));
                message.message_id = format!("{}-{offset}", target.workspace_id);
                router
                    .enqueue(message.clone())
                    .await
                    .map_err(|error| error.to_string())?;
                messages.push(message);
            }
            let abandoned = router
                .claim_next(target)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("{} first claim missing", target.workspace_id))?;
            assert_eq!(abandoned.attempt, 1);
        }
        drop(router);

        let alpha = targets
            .first()
            .cloned()
            .ok_or_else(|| "alpha target missing".to_string())?;
        let beta = targets
            .get(1)
            .cloned()
            .ok_or_else(|| "beta target missing".to_string())?;
        let gamma = targets
            .get(2)
            .cloned()
            .ok_or_else(|| "gamma target missing".to_string())?;
        let (alpha_count, beta_count, gamma_count) = tokio::try_join!(
            drain_inbox(root.clone(), alpha),
            drain_inbox(root.clone(), beta),
            drain_inbox(root.clone(), gamma),
        )?;
        assert_eq!(alpha_count, MESSAGES_PER_WORKSPACE);
        assert_eq!(beta_count, MESSAGES_PER_WORKSPACE);
        assert_eq!(gamma_count, MESSAGES_PER_WORKSPACE);

        let restarted = AgentRouter::new(root);
        for target in &targets {
            let records = restarted
                .records(target)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(records.len(), MESSAGES_PER_WORKSPACE);
            assert!(
                records
                    .iter()
                    .all(|record| record.status == AgentDeliveryStatus::Delivered)
            );
            assert_eq!(
                records.iter().filter(|record| record.attempt == 2).count(),
                1
            );
        }
        for message in messages {
            let duplicate = restarted
                .enqueue(message)
                .await
                .map_err(|error| error.to_string())?;
            assert!(duplicate.duplicate);
            assert_eq!(duplicate.status, AgentDeliveryStatus::Delivered);
        }
        Ok(())
    }
}
