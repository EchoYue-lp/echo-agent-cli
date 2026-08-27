//! Durable application-owned messaging between workspace conversations.
//!
//! The router persists accepted messages before any wake attempt. It does not
//! write conversation transcripts and does not own an Agent executor; later
//! delivery stages must invoke the existing chat driver for the target host.

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
    InjectionStarted,
    Injected,
    Delivered,
    Failed,
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
    pub status: AgentDeliveryStatus,
    pub accepted_at: DateTime<Utc>,
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
    pub status: AgentDeliveryStatus,
    pub turn_id: String,
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
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
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
    InjectionStarted {
        message_id: String,
        attempt_id: String,
        started_at: DateTime<Utc>,
        turn_id: String,
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
    #[error(
        "Agent inbox is retiring for workspace '{workspace_id}' conversation {conversation_id:?}"
    )]
    Retiring {
        workspace_id: String,
        conversation_id: Option<String>,
    },
    #[error(
        "Agent inbox batch '{batch_id}' was not committed after {attempts} attempt(s): {detail}"
    )]
    AppendNotCommitted {
        batch_id: String,
        attempts: usize,
        detail: String,
    },
    #[error("Agent inbox batch '{batch_id}' has an unresolved commit outcome: {detail}")]
    AppendOutcomeUnknown { batch_id: String, detail: String },
    #[error("Agent inbox batch '{batch_id}' conflicts with persisted identity: {detail}")]
    AppendIdentityConflict { batch_id: String, detail: String },
}

type AgentInboxReducer =
    CheckpointedReducer<SegmentedFileEventJournal<AgentInboxEvent>, AgentInboxProjection>;

struct AgentInboxAuthorityState {
    journal: Arc<SegmentedFileEventJournal<AgentInboxEvent>>,
    reducer: AgentInboxReducer,
    durability_debt: Option<String>,
}

struct AgentInboxAuthority {
    directory: PathBuf,
    checkpoint_path: PathBuf,
    expected_target: AgentAddress,
    operation: StdMutex<()>,
    state: StdMutex<Option<AgentInboxAuthorityState>>,
}

pub struct AgentRouterRetirementGuard {
    _marker: Arc<AgentRouterRetirementMarker>,
    root: PathBuf,
    inboxes: Arc<AgentInboxRegistry>,
    scope: AgentRouterRetirementScope,
}

#[derive(Clone)]
enum AgentRouterRetirementScope {
    Target(AgentAddress),
    Workspace(WorkspaceId),
}

struct AgentRouterRetirementMarker {
    registry: Arc<AgentInboxRegistry>,
    target: Option<AgentAddress>,
    workspace_id: Option<WorkspaceId>,
}

impl Drop for AgentRouterRetirementMarker {
    fn drop(&mut self) {
        let _lifecycle = self
            .registry
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(target) = self.target.take() {
            self.registry.retiring_targets.remove(&target);
        }
        if let Some(workspace_id) = self.workspace_id.take() {
            self.registry.retiring_workspaces.remove(&workspace_id);
        }
    }
}

impl AgentRouterRetirementGuard {
    pub async fn purge(&self) -> Result<(), AgentRouterError> {
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let scope = self.scope.clone();
        tokio::task::spawn_blocking(move || match scope {
            AgentRouterRetirementScope::Target(target) => {
                retire_target_sync(&root, &inboxes, &target)
            }
            AgentRouterRetirementScope::Workspace(workspace_id) => {
                retire_workspace_sync(&root, &inboxes, &workspace_id)
            }
        })
        .await
        .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }
}

/// File-backed durable inbox owner.
pub struct AgentRouter {
    root: PathBuf,
    inboxes: Arc<AgentInboxRegistry>,
}

#[derive(Default)]
struct AgentInboxRegistry {
    lifecycle: StdMutex<()>,
    authorities: DashMap<AgentAddress, Arc<AgentInboxAuthority>>,
    retiring_targets: DashMap<AgentAddress, ()>,
    retiring_workspaces: DashMap<WorkspaceId, ()>,
}

#[derive(Default)]
struct AgentDeliverySupervisorState {
    active: HashMap<AgentAddress, u64>,
    dirty: HashMap<AgentAddress, u64>,
    next_driver_generation: u64,
    drivers: tokio::task::JoinSet<()>,
    driver_targets: HashMap<tokio::task::Id, AgentAddress>,
    driver_failures: Vec<String>,
    retiring_targets: HashSet<AgentAddress>,
    retiring_workspaces: HashSet<WorkspaceId>,
    shutting_down: bool,
}

struct AgentDeliveryDriverGuard {
    state: Arc<StdMutex<AgentDeliverySupervisorState>>,
    idle: Arc<tokio::sync::Notify>,
    target: AgentAddress,
    generation: u64,
    recover: Arc<dyn Fn(AgentAddress) + Send + Sync>,
}

pub(crate) struct AgentDeliveryDriverCycle {
    state: Arc<StdMutex<AgentDeliverySupervisorState>>,
    idle: Arc<tokio::sync::Notify>,
    target: AgentAddress,
    generation: u64,
}

impl AgentDeliveryDriverCycle {
    /// Complete one drain cycle for this exact driver generation. `true` means
    /// an enqueue raced the cycle and the same owner must inspect the target
    /// again before releasing it.
    pub(crate) fn complete(&self) -> Result<bool, AgentRouterError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        if state.active.get(&self.target) != Some(&self.generation) {
            return Ok(false);
        }
        if state.dirty.get(&self.target) == Some(&self.generation) && !state.shutting_down {
            state.dirty.remove(&self.target);
            return Ok(true);
        }
        state.active.remove(&self.target);
        if state.dirty.get(&self.target) == Some(&self.generation) {
            state.dirty.remove(&self.target);
        }
        self.idle.notify_waiters();
        Ok(false)
    }
}

impl Drop for AgentDeliveryDriverGuard {
    fn drop(&mut self) {
        let mut recover = false;
        if let Ok(mut state) = self.state.lock()
            && state.active.get(&self.target) == Some(&self.generation)
        {
            state.active.remove(&self.target);
            if state.dirty.get(&self.target) == Some(&self.generation) {
                state.dirty.remove(&self.target);
                recover = !state.shutting_down;
            }
        }
        if recover {
            (self.recover)(self.target.clone());
        }
        self.idle.notify_waiters();
    }
}

pub struct AgentDeliveryRetirementGuard {
    state: Arc<StdMutex<AgentDeliverySupervisorState>>,
    idle: Arc<tokio::sync::Notify>,
    target: AgentAddress,
}

pub struct AgentDeliveryWorkspaceRetirementGuard {
    state: Arc<StdMutex<AgentDeliverySupervisorState>>,
    idle: Arc<tokio::sync::Notify>,
    workspace_id: WorkspaceId,
}

impl Drop for AgentDeliveryRetirementGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.retiring_targets.remove(&self.target);
        }
        self.idle.notify_waiters();
    }
}

impl Drop for AgentDeliveryWorkspaceRetirementGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.retiring_workspaces.remove(&self.workspace_id);
        }
        self.idle.notify_waiters();
    }
}

/// Application-owned lifetime manager for asynchronous inbox delivery.
/// It owns task lifetimes only; Agent execution remains in `drive_chat`.
pub struct AgentDeliverySupervisor {
    state: Arc<StdMutex<AgentDeliverySupervisorState>>,
    idle: Arc<tokio::sync::Notify>,
    cancel: echo_agent::agent::CancellationToken,
}

impl Default for AgentDeliverySupervisor {
    fn default() -> Self {
        Self {
            state: Arc::new(StdMutex::new(AgentDeliverySupervisorState::default())),
            idle: Arc::new(tokio::sync::Notify::new()),
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
                    .keys()
                    .any(|target| &target.workspace_id == workspace_id)
            })
            .unwrap_or(true)
    }

    pub fn has_active_target(&self, target: &AgentAddress) -> bool {
        self.state
            .lock()
            .map(|state| state.active.contains_key(target))
            .unwrap_or(true)
    }

    #[cfg(test)]
    fn is_retiring_target(&self, target: &AgentAddress) -> bool {
        self.state
            .lock()
            .map(|state| state.retiring_targets.contains(target))
            .unwrap_or(false)
    }

    /// Start one target-owned delivery task or mark the already-running task
    /// dirty so it performs another empty-inbox check before exit.
    pub(crate) fn supervise<Factory, Operation>(
        &self,
        target: AgentAddress,
        recover: Arc<dyn Fn(AgentAddress) + Send + Sync>,
        operation: Factory,
    ) -> Result<bool, AgentRouterError>
    where
        Factory: FnOnce(AgentDeliveryDriverCycle) -> Operation + Send + 'static,
        Operation: std::future::Future<Output = ()> + Send + 'static,
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
        if state.retiring_targets.contains(&target)
            || state.retiring_workspaces.contains(&target.workspace_id)
        {
            return Err(AgentRouterError::Retiring {
                workspace_id: target.workspace_id.to_string(),
                conversation_id: Some(target.conversation_id),
            });
        }
        if let Some(generation) = state.active.get(&target).copied() {
            state.dirty.insert(target, generation);
            return Ok(false);
        }
        let generation = state
            .next_driver_generation
            .checked_add(1)
            .ok_or_else(|| AgentRouterError::Task("delivery driver generation exhausted".into()))?;
        state.next_driver_generation = generation;
        state.active.insert(target.clone(), generation);
        let guard = AgentDeliveryDriverGuard {
            state: Arc::clone(&self.state),
            idle: Arc::clone(&self.idle),
            target: target.clone(),
            generation,
            recover,
        };
        let cycle = AgentDeliveryDriverCycle {
            state: Arc::clone(&self.state),
            idle: Arc::clone(&self.idle),
            target: target.clone(),
            generation,
        };
        let abort = state.drivers.spawn_on(
            async move {
                let _guard = guard;
                operation(cycle).await;
            },
            &runtime,
        );
        state.driver_targets.insert(abort.id(), target);
        Ok(true)
    }

    pub async fn retire_target(
        &self,
        target: AgentAddress,
    ) -> Result<AgentDeliveryRetirementGuard, AgentRouterError> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            Self::collect_finished(&mut state);
            if state.shutting_down {
                return Err(AgentRouterError::ShuttingDown);
            }
            if !state.retiring_targets.insert(target.clone()) {
                return Err(AgentRouterError::Retiring {
                    workspace_id: target.workspace_id.to_string(),
                    conversation_id: Some(target.conversation_id),
                });
            }
        }
        let guard = AgentDeliveryRetirementGuard {
            state: Arc::clone(&self.state),
            idle: Arc::clone(&self.idle),
            target: target.clone(),
        };
        loop {
            let notified = self.idle.notified();
            let active = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?
                .active
                .contains_key(&target);
            if !active {
                return Ok(guard);
            }
            notified.await;
        }
    }

    pub async fn retire_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<AgentDeliveryWorkspaceRetirementGuard, AgentRouterError> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            Self::collect_finished(&mut state);
            if state.shutting_down {
                return Err(AgentRouterError::ShuttingDown);
            }
            if !state.retiring_workspaces.insert(workspace_id.clone())
                || state
                    .retiring_targets
                    .iter()
                    .any(|target| target.workspace_id == workspace_id)
            {
                state.retiring_workspaces.remove(&workspace_id);
                return Err(AgentRouterError::Retiring {
                    workspace_id: workspace_id.to_string(),
                    conversation_id: None,
                });
            }
        }
        let guard = AgentDeliveryWorkspaceRetirementGuard {
            state: Arc::clone(&self.state),
            idle: Arc::clone(&self.idle),
            workspace_id: workspace_id.clone(),
        };
        loop {
            let notified = self.idle.notified();
            let active = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?
                .active
                .keys()
                .any(|target| target.workspace_id == workspace_id);
            if !active {
                return Ok(guard);
            }
            notified.await;
        }
    }

    fn collect_finished(state: &mut AgentDeliverySupervisorState) {
        while let Some(result) = state.drivers.try_join_next_with_id() {
            match result {
                Ok((driver_id, ())) => {
                    state.driver_targets.remove(&driver_id);
                }
                Err(error) => {
                    let target = state.driver_targets.remove(&error.id());
                    let failure = target.map_or_else(
                        || format!("Agent delivery task failed to join: {error}"),
                        |target| {
                            format!("Agent delivery task for {target:?} failed to join: {error}")
                        },
                    );
                    tracing::error!(error = %failure, "Agent delivery task failed to join");
                    state.driver_failures.push(failure);
                }
            }
        }
    }

    /// Permanently close delivery admission and broadcast cancellation without
    /// waiting for any driver. Application shutdown calls this in its first
    /// phase so dependent foreground owners can observe cancellation before any
    /// lifecycle join begins.
    pub fn close_admission_and_cancel(&self) -> Result<(), AgentRouterError> {
        self.cancel.cancel();
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        state.shutting_down = true;
        state.dirty.clear();
        state.retiring_targets.clear();
        state.retiring_workspaces.clear();
        Ok(())
    }

    /// Join every delivery driver accepted before admission closed.
    pub async fn join(&self) -> Result<(), AgentRouterError> {
        let (mut drivers, mut driver_targets, mut failures) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            state.shutting_down = true;
            state.active.clear();
            state.dirty.clear();
            state.retiring_targets.clear();
            state.retiring_workspaces.clear();
            (
                std::mem::take(&mut state.drivers),
                std::mem::take(&mut state.driver_targets),
                std::mem::take(&mut state.driver_failures),
            )
        };
        while let Some(result) = drivers.join_next_with_id().await {
            match result {
                Ok((driver_id, ())) => {
                    driver_targets.remove(&driver_id);
                }
                Err(error) => {
                    let target = driver_targets.remove(&error.id());
                    failures.push(target.map_or_else(
                        || format!("Agent delivery task failed to join: {error}"),
                        |target| {
                            format!("Agent delivery task for {target:?} failed to join: {error}")
                        },
                    ));
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AgentRouterError::Task(failures.join("; ")))
        }
    }

    pub async fn shutdown(&self) -> Result<(), AgentRouterError> {
        self.close_admission_and_cancel()?;
        self.join().await
    }
}

impl AgentRouter {
    pub fn at_default_root() -> Arc<Self> {
        Arc::new(Self::new(crate::data_root::user_data_path("agent-router")))
    }

    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            inboxes: Arc::new(AgentInboxRegistry::default()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn begin_target_retirement(
        &self,
        target: AgentAddress,
    ) -> Result<AgentRouterRetirementGuard, AgentRouterError> {
        target.validate()?;
        {
            let _lifecycle = self
                .inboxes
                .lifecycle
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            if self
                .inboxes
                .retiring_workspaces
                .contains_key(&target.workspace_id)
                || self
                    .inboxes
                    .retiring_targets
                    .insert(target.clone(), ())
                    .is_some()
            {
                return Err(AgentRouterError::Retiring {
                    workspace_id: target.workspace_id.to_string(),
                    conversation_id: Some(target.conversation_id),
                });
            }
        }
        let marker = Arc::new(AgentRouterRetirementMarker {
            registry: Arc::clone(&self.inboxes),
            target: Some(target.clone()),
            workspace_id: None,
        });
        let guard = AgentRouterRetirementGuard {
            _marker: marker,
            root: self.root.clone(),
            inboxes: Arc::clone(&self.inboxes),
            scope: AgentRouterRetirementScope::Target(target),
        };
        Ok(guard)
    }

    pub async fn retire_target(
        &self,
        target: AgentAddress,
    ) -> Result<AgentRouterRetirementGuard, AgentRouterError> {
        let guard = self.begin_target_retirement(target)?;
        guard.purge().await?;
        Ok(guard)
    }

    pub fn begin_workspace_retirement(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<AgentRouterRetirementGuard, AgentRouterError> {
        {
            let _lifecycle = self
                .inboxes
                .lifecycle
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            if self.inboxes.retiring_workspaces.contains_key(&workspace_id)
                || self
                    .inboxes
                    .retiring_targets
                    .iter()
                    .any(|entry| entry.key().workspace_id == workspace_id)
            {
                return Err(AgentRouterError::Retiring {
                    workspace_id: workspace_id.to_string(),
                    conversation_id: None,
                });
            }
            self.inboxes
                .retiring_workspaces
                .insert(workspace_id.clone(), ());
        }
        let marker = Arc::new(AgentRouterRetirementMarker {
            registry: Arc::clone(&self.inboxes),
            target: None,
            workspace_id: Some(workspace_id.clone()),
        });
        let guard = AgentRouterRetirementGuard {
            _marker: marker,
            root: self.root.clone(),
            inboxes: Arc::clone(&self.inboxes),
            scope: AgentRouterRetirementScope::Workspace(workspace_id),
        };
        Ok(guard)
    }

    pub async fn retire_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<AgentRouterRetirementGuard, AgentRouterError> {
        let guard = self.begin_workspace_retirement(workspace_id)?;
        guard.purge().await?;
        Ok(guard)
    }

    pub async fn list_groups(&self) -> Result<Vec<AgentGroup>, AgentRouterError> {
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
        let root = self.root.clone();
        let group_id = group_id.to_string();
        tokio::task::spawn_blocking(move || delete_group_sync(&root, &group_id))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    /// Persist a message once within the retained target inbox window.
    /// Repeating a retained `message_id` returns the original acceptance;
    /// identities evicted by either terminal retention bound may be admitted
    /// again immediately after eviction.
    pub async fn enqueue(
        &self,
        message: AgentMessage,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        message.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        tokio::task::spawn_blocking(move || enqueue_sync(&root, &inboxes, message))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn pending(
        &self,
        target: &AgentAddress,
    ) -> Result<Vec<AgentMessage>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || pending_sync(&root, &inboxes, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn claim_next(
        &self,
        target: &AgentAddress,
    ) -> Result<Option<AgentDeliveryClaim>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || claim_next_sync(&root, &inboxes, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn next_attempt_at(
        &self,
        target: &AgentAddress,
    ) -> Result<Option<DateTime<Utc>>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || next_attempt_at_sync(&root, &inboxes, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    /// Return the exact non-terminal attempt whose input already reached model
    /// context. Cold recovery must reconcile this attempt against transcript
    /// facts and must never create a new claim that could replay side effects.
    pub async fn in_flight_claim(
        &self,
        target: &AgentAddress,
    ) -> Result<Option<AgentDeliveryInFlight>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || in_flight_claim_sync(&root, &inboxes, &target))
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

    pub async fn begin_injection(
        &self,
        claim: &AgentDeliveryClaim,
        turn_id: impl Into<String>,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        self.settle_claim(
            claim,
            ClaimSettlement::InjectionStarted {
                turn_id: turn_id.into(),
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

    /// Return the retained terminal window followed by the complete frontier.
    pub async fn records(
        &self,
        target: &AgentAddress,
    ) -> Result<Vec<AgentDeliveryRecord>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || records_sync(&root, &inboxes, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    #[cfg(test)]
    pub(crate) async fn event_phases_for_test(
        &self,
        target: &AgentAddress,
        message_id: &str,
    ) -> Result<Vec<&'static str>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        let message_id = message_id.to_string();
        tokio::task::spawn_blocking(move || {
            let authority = authority_for(&root, &inboxes, &target)?;
            let guard = authority
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            let state = guard.as_ref().ok_or_else(|| AgentRouterError::Corrupt {
                path: authority.directory.clone(),
                message: "Agent inbox authority is closed".to_string(),
            })?;
            let mut after = 0;
            let mut phases = Vec::new();
            loop {
                let records = state
                    .journal
                    .replay_after(after, 256)
                    .map_err(|error| journal_error(&authority.directory, error))?;
                if records.is_empty() {
                    break;
                }
                for record in records {
                    after = record.sequence;
                    let (event_message_id, phase) = match record.event.as_ref() {
                        AgentInboxEvent::Accepted { message, .. } => {
                            (message.message_id.as_str(), "accepted")
                        }
                        AgentInboxEvent::Claimed { message_id, .. } => {
                            (message_id.as_str(), "claimed")
                        }
                        AgentInboxEvent::InjectionStarted { message_id, .. } => {
                            (message_id.as_str(), "injection_started")
                        }
                        AgentInboxEvent::Injected { message_id, .. } => {
                            (message_id.as_str(), "injected")
                        }
                        AgentInboxEvent::Deferred { message_id, .. } => {
                            (message_id.as_str(), "deferred")
                        }
                        AgentInboxEvent::Delivered { message_id, .. } => {
                            (message_id.as_str(), "delivered")
                        }
                        AgentInboxEvent::Failed { message_id, .. } => {
                            (message_id.as_str(), "failed")
                        }
                    };
                    if event_message_id == message_id {
                        phases.push(phase);
                    }
                }
            }
            Ok(phases)
        })
        .await
        .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    async fn settle_claim(
        &self,
        claim: &AgentDeliveryClaim,
        settlement: ClaimSettlement,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let claim = claim.clone();
        tokio::task::spawn_blocking(move || settle_claim_sync(&root, &inboxes, &claim, settlement))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }
}

enum ClaimSettlement {
    InjectionStarted {
        turn_id: String,
    },
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

impl AgentInboxAuthority {
    fn open(root: &Path, target: &AgentAddress) -> Result<Arc<Self>, AgentRouterError> {
        let inbox = inbox_dir(root, target);
        let directory = inbox.join("journal");
        let checkpoint_path = inbox.join("projection.checkpoint.json");
        let state = Self::open_state(&directory, &checkpoint_path, target)?;
        Ok(Arc::new(Self {
            directory,
            checkpoint_path,
            expected_target: target.clone(),
            operation: StdMutex::new(()),
            state: StdMutex::new(Some(state)),
        }))
    }

    fn open_state(
        directory: &Path,
        checkpoint_path: &Path,
        target: &AgentAddress,
    ) -> Result<AgentInboxAuthorityState, AgentRouterError> {
        let journal = Arc::new(
            SegmentedFileEventJournal::open(
                directory,
                INBOX_SEGMENT_BYTES,
                FileDurability::SyncData,
            )
            .map_err(|error| journal_error(directory, error))?,
        );
        let checkpoints = Arc::new(FileCheckpointStore::open(checkpoint_path));
        let reducer = CheckpointedReducer::new(
            Arc::clone(&journal),
            checkpoints as Arc<dyn CheckpointStore<AgentInboxProjection>>,
            INBOX_CHECKPOINT_EVERY,
        );
        reducer
            .recover()
            .map_err(|error| journal_error(directory, error))?;
        reducer.with_state(|projection| projection.validate(directory, target))?;
        Ok(AgentInboxAuthorityState {
            journal,
            reducer,
            durability_debt: None,
        })
    }

    fn with_projection<T>(
        &self,
        operation: impl FnOnce(&AgentInboxProjection) -> Result<T, AgentRouterError>,
    ) -> Result<T, AgentRouterError> {
        let guard = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        let state = guard.as_ref().ok_or_else(|| AgentRouterError::Corrupt {
            path: self.directory.clone(),
            message: "Agent inbox authority is closed".to_string(),
        })?;
        state.reducer.with_state(|projection| operation(projection))
    }

    fn lock_operation(&self) -> Result<std::sync::MutexGuard<'_, ()>, AgentRouterError> {
        self.operation
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)
    }

    fn append(&self, event: AgentInboxEvent) -> Result<JournalDurabilityStatus, AgentRouterError> {
        let prepared =
            PreparedJournalBatch::new(vec![event]).map_err(|error| AgentRouterError::Corrupt {
                path: self.directory.clone(),
                message: error.to_string(),
            })?;
        let batch_id = prepared.batch_id().to_string();
        let mut prepared = Some(prepared);
        let mut attempts = 0_usize;
        let mut guard = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        loop {
            attempts = attempts.saturating_add(1);
            let state = guard.as_mut().ok_or_else(|| AgentRouterError::Corrupt {
                path: self.directory.clone(),
                message: "Agent inbox authority is closed".to_string(),
            })?;
            Self::retry_durability_debt(state, &self.directory);
            let batch = prepared.take().ok_or_else(|| AgentRouterError::Corrupt {
                path: self.directory.clone(),
                message: "prepared Agent inbox batch ownership was lost".to_string(),
            })?;
            let mut receipt = match state.reducer.apply_batch(batch) {
                Ok(receipt) => receipt,
                Err(CheckpointedApplyError::Journal(JournalBatchAppendError::NotCommitted {
                    batch,
                    error,
                })) if attempts < MAX_INBOX_APPEND_ATTEMPTS => {
                    prepared = Some(batch);
                    tracing::warn!(%batch_id, attempts, %error, "retrying uncommitted Agent inbox batch");
                    continue;
                }
                Err(CheckpointedApplyError::Journal(JournalBatchAppendError::NotCommitted {
                    error,
                    ..
                })) => {
                    return Err(AgentRouterError::AppendNotCommitted {
                        batch_id,
                        attempts,
                        detail: error,
                    });
                }
                Err(CheckpointedApplyError::Journal(error)) if error.requires_reopen() => {
                    let detail = error.to_string();
                    let batch = error.into_prepared().ok_or_else(|| {
                        AgentRouterError::AppendOutcomeUnknown {
                            batch_id: batch_id.clone(),
                            detail: "journal did not return prepared batch ownership".to_string(),
                        }
                    })?;
                    let stale = guard.take();
                    drop(stale);
                    let reopened = Self::open_state(
                        &self.directory,
                        &self.checkpoint_path,
                        &self.expected_target,
                    )
                    .map_err(|error| {
                        AgentRouterError::AppendOutcomeUnknown {
                            batch_id: batch_id.clone(),
                            detail: format!("{detail}; verified reopen failed: {error}"),
                        }
                    })?;
                    match reopened.journal.lookup_batch(&batch).map_err(|error| {
                        AgentRouterError::AppendOutcomeUnknown {
                            batch_id: batch_id.clone(),
                            detail: format!("{detail}; lookup failed: {error}"),
                        }
                    })? {
                        JournalBatchLookup::AlreadyCommitted(_) => {
                            *guard = Some(reopened);
                            prepared = Some(batch);
                            continue;
                        }
                        JournalBatchLookup::Absent if attempts < MAX_INBOX_APPEND_ATTEMPTS => {
                            *guard = Some(reopened);
                            prepared = Some(batch);
                            continue;
                        }
                        JournalBatchLookup::Absent => {
                            return Err(AgentRouterError::AppendOutcomeUnknown {
                                batch_id,
                                detail: format!(
                                    "{detail}; batch remained absent after {attempts} attempts"
                                ),
                            });
                        }
                        JournalBatchLookup::Conflict { error } => {
                            return Err(AgentRouterError::AppendIdentityConflict {
                                batch_id,
                                detail: error,
                            });
                        }
                    }
                }
                Err(CheckpointedApplyError::Journal(error)) => {
                    return Err(AgentRouterError::AppendIdentityConflict {
                        batch_id,
                        detail: error.to_string(),
                    });
                }
                Err(CheckpointedApplyError::CommittedInvariant { error, .. }) => {
                    let stale = guard.take();
                    drop(stale);
                    return Err(AgentRouterError::AppendOutcomeUnknown {
                        batch_id,
                        detail: error,
                    });
                }
                Err(CheckpointedApplyError::Prepare(error)) => {
                    return Err(AgentRouterError::Corrupt {
                        path: self.directory.clone(),
                        message: error.to_string(),
                    });
                }
            };
            state
                .reducer
                .with_state(|projection| projection.ensure_incremental_valid(&self.directory))?;
            match &receipt.journal {
                JournalDurabilityStatus::Confirmed => state.durability_debt = None,
                JournalDurabilityStatus::Unconfirmed => {
                    state.durability_debt = Some(format!(
                        "Agent inbox batch {} has unconfirmed durability",
                        receipt.batch_id
                    ));
                }
                JournalDurabilityStatus::Degraded { error } => {
                    state.durability_debt = Some(error.clone());
                }
            }
            Self::retry_durability_debt(state, &self.directory);
            receipt.journal = state
                .durability_debt
                .clone()
                .map_or(JournalDurabilityStatus::Confirmed, |error| {
                    JournalDurabilityStatus::Degraded { error }
                });
            if let CheckpointApplyStatus::Degraded { error } = &receipt.checkpoint {
                tracing::warn!(path = %self.checkpoint_path.display(), %error, "Agent inbox checkpoint write is degraded; authoritative journal remains committed");
            }
            Self::maintain_retention(state, &self.directory);
            return Ok(receipt.journal);
        }
    }

    fn retry_durability_debt(state: &mut AgentInboxAuthorityState, directory: &Path) {
        if state.durability_debt.is_some() {
            match state.journal.sync_data() {
                Ok(()) => state.durability_debt = None,
                Err(error) => {
                    state.durability_debt = Some(error.to_string());
                    tracing::warn!(path = %directory.display(), %error, "Agent inbox durability debt remains pending");
                }
            }
        }
    }

    fn maintain_retention(state: &mut AgentInboxAuthorityState, directory: &Path) {
        let segments = state.journal.segments();
        if segments.len() <= INBOX_MAX_SEGMENTS {
            return;
        }
        if let Err(error) = state.reducer.checkpoint() {
            tracing::warn!(path = %directory.display(), %error, "Agent inbox checkpoint compaction is degraded");
            return;
        }
        let keep_from = segments
            .get(segments.len().saturating_sub(INBOX_MAX_SEGMENTS))
            .map(|segment| segment.start_sequence)
            .unwrap_or(1);
        if let Err(error) = state.journal.prune_closed_segments_before(keep_from) {
            tracing::warn!(path = %directory.display(), %error, "Agent inbox segment cleanup remains pending");
        }
    }

    fn close(&self) -> Result<(), AgentRouterError> {
        let _operation = self.lock_operation()?;
        let stale = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?
            .take();
        drop(stale);
        Ok(())
    }
}

fn authority_for(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Arc<AgentInboxAuthority>, AgentRouterError> {
    {
        let _lifecycle = inboxes
            .lifecycle
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        ensure_inbox_not_retiring(inboxes, target)?;
        if let Some(existing) = inboxes.authorities.get(target) {
            return Ok(Arc::clone(existing.value()));
        }
    }
    let opened = AgentInboxAuthority::open(root, target)?;
    let _lifecycle = inboxes
        .lifecycle
        .lock()
        .map_err(|_| AgentRouterError::StateUnavailable)?;
    ensure_inbox_not_retiring(inboxes, target)?;
    let entry = inboxes
        .authorities
        .entry(target.clone())
        .or_insert_with(|| Arc::clone(&opened));
    Ok(Arc::clone(entry.value()))
}

fn ensure_inbox_not_retiring(
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<(), AgentRouterError> {
    if inboxes.retiring_targets.contains_key(target)
        || inboxes
            .retiring_workspaces
            .contains_key(&target.workspace_id)
    {
        Err(AgentRouterError::Retiring {
            workspace_id: target.workspace_id.to_string(),
            conversation_id: Some(target.conversation_id.clone()),
        })
    } else {
        Ok(())
    }
}

fn retire_target_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<(), AgentRouterError> {
    if let Some((_, authority)) = inboxes.authorities.remove(target) {
        authority.close()?;
    }
    let path = inbox_dir(root, target);
    match std::fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AgentRouterError::Io { path, source }),
    }
}

fn retire_workspace_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    workspace_id: &WorkspaceId,
) -> Result<(), AgentRouterError> {
    let targets = inboxes
        .authorities
        .iter()
        .filter(|entry| &entry.key().workspace_id == workspace_id)
        .map(|entry| entry.key().clone())
        .collect::<Vec<_>>();
    for target in targets {
        if let Some((_, authority)) = inboxes.authorities.remove(&target) {
            authority.close()?;
        }
    }
    let path = root
        .join("inboxes")
        .join(stable_segment(workspace_id.as_str()));
    match std::fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AgentRouterError::Io { path, source }),
    }
}

fn enqueue_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    message: AgentMessage,
) -> Result<AgentDeliveryReceipt, AgentRouterError> {
    let target = message.to.clone();
    let authority = authority_for(root, inboxes, &target)?;
    let _operation = authority.lock_operation()?;
    let _lifecycle = inboxes
        .lifecycle
        .lock()
        .map_err(|_| AgentRouterError::StateUnavailable)?;
    ensure_inbox_not_retiring(inboxes, &target)?;
    let existing = authority
        .with_projection(|projection| Ok(projection.message(&message.message_id).cloned()))?;
    if let Some(existing) = existing {
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
            durability: AgentDeliveryDurability::Unconfirmed,
        });
    }

    let accepted_at = Utc::now();
    let durability = authority.append(AgentInboxEvent::Accepted {
        message: message.clone(),
        accepted_at,
    })?;
    Ok(AgentDeliveryReceipt {
        message_id: message.message_id,
        target: message.to,
        status: AgentDeliveryStatus::Queued,
        accepted_at,
        duplicate: false,
        durability: durability.into(),
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

fn pending_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Vec<AgentMessage>, AgentRouterError> {
    let authority = authority_for(root, inboxes, target)?;
    authority.with_projection(|projection| {
        Ok(projection
            .frontier_entries()
            .map(|entry| entry.message.clone())
            .collect())
    })
}

fn claim_next_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Option<AgentDeliveryClaim>, AgentRouterError> {
    let authority = authority_for(root, inboxes, target)?;
    let _operation = authority.lock_operation()?;
    let _lifecycle = inboxes
        .lifecycle
        .lock()
        .map_err(|_| AgentRouterError::StateUnavailable)?;
    ensure_inbox_not_retiring(inboxes, target)?;
    let next = authority.with_projection(|projection| Ok(projection.frontier_entry().cloned()))?;
    let Some(next) = next else {
        return Ok(None);
    };
    if matches!(
        next.status,
        AgentDeliveryStatus::InjectionStarted | AgentDeliveryStatus::Injected
    ) {
        return Ok(None);
    }
    if next
        .next_attempt_at
        .is_some_and(|deadline| deadline > Utc::now())
    {
        return Ok(None);
    }
    let attempt = next.attempt.saturating_add(1);
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let claimed_at = Utc::now();
    authority.append(AgentInboxEvent::Claimed {
        message_id: next.message.message_id.clone(),
        attempt_id: attempt_id.clone(),
        attempt,
        claimed_at,
    })?;
    Ok(Some(AgentDeliveryClaim {
        message: next.message,
        attempt_id,
        attempt,
        claimed_at,
    }))
}

fn in_flight_claim_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Option<AgentDeliveryInFlight>, AgentRouterError> {
    let authority = authority_for(root, inboxes, target)?;
    authority.with_projection(|projection| {
        let Some(entry) = projection.frontier_entry().cloned() else {
            return Ok(None);
        };
        if !matches!(
            entry.status,
            AgentDeliveryStatus::InjectionStarted | AgentDeliveryStatus::Injected
        ) {
            return Ok(None);
        }
        let attempt_id = entry.attempt_id.ok_or_else(|| {
            corrupt_event(
                &authority.directory,
                format!(
                    "injected message {} has no attempt identity",
                    entry.message.message_id
                ),
            )
        })?;
        let claimed_at = entry.claimed_at.ok_or_else(|| {
            corrupt_event(
                &authority.directory,
                format!(
                    "injected message {} has no claim timestamp",
                    entry.message.message_id
                ),
            )
        })?;
        let turn_id = entry.turn_id.ok_or_else(|| {
            corrupt_event(
                &authority.directory,
                "in-flight delivery has no turn identity".to_string(),
            )
        })?;
        Ok(Some(AgentDeliveryInFlight {
            claim: AgentDeliveryClaim {
                message: entry.message,
                attempt_id,
                attempt: entry.attempt,
                claimed_at,
            },
            status: entry.status,
            turn_id,
        }))
    })
}

fn settle_claim_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    claim: &AgentDeliveryClaim,
    settlement: ClaimSettlement,
) -> Result<AgentDeliveryReceipt, AgentRouterError> {
    let target = claim.message.to.clone();
    let authority = authority_for(root, inboxes, &target)?;
    let _operation = authority.lock_operation()?;
    let entry = authority.with_projection(|projection| {
        projection
            .message(&claim.message.message_id)
            .cloned()
            .ok_or_else(|| AgentRouterError::StaleClaim {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
            })
    })?;
    let valid_phase = match &settlement {
        ClaimSettlement::InjectionStarted { .. } => entry.status == AgentDeliveryStatus::Claimed,
        ClaimSettlement::Injected { turn_id } => {
            entry.status == AgentDeliveryStatus::InjectionStarted
                && entry.turn_id.as_deref() == Some(turn_id)
        }
        ClaimSettlement::Deferred { .. } => matches!(
            entry.status,
            AgentDeliveryStatus::Claimed | AgentDeliveryStatus::InjectionStarted
        ),
        ClaimSettlement::Delivered { turn_id, .. } => {
            entry.status == AgentDeliveryStatus::Injected
                && entry.turn_id.as_deref() == Some(turn_id)
        }
        ClaimSettlement::Failed { .. } => matches!(
            entry.status,
            AgentDeliveryStatus::Claimed
                | AgentDeliveryStatus::InjectionStarted
                | AgentDeliveryStatus::Injected
        ),
    };
    if entry.attempt_id.as_deref() != Some(claim.attempt_id.as_str()) || !valid_phase {
        return Err(AgentRouterError::StaleClaim {
            message_id: claim.message.message_id.clone(),
            attempt_id: claim.attempt_id.clone(),
        });
    }
    let (status, event) = match settlement {
        ClaimSettlement::InjectionStarted { turn_id } => {
            let event = AgentInboxEvent::InjectionStarted {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                started_at: Utc::now(),
                turn_id,
            };
            (AgentDeliveryStatus::InjectionStarted, event)
        }
        ClaimSettlement::Injected { turn_id } => {
            let event = AgentInboxEvent::Injected {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                injected_at: Utc::now(),
                turn_id,
            };
            (AgentDeliveryStatus::Injected, event)
        }
        ClaimSettlement::Deferred {
            reason,
            next_attempt_at,
        } => {
            let event = AgentInboxEvent::Deferred {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                deferred_at: Utc::now(),
                reason,
                next_attempt_at: Some(next_attempt_at),
            };
            (AgentDeliveryStatus::Queued, event)
        }
        ClaimSettlement::Delivered {
            turn_id,
            reply_message_id,
        } => {
            let event = AgentInboxEvent::Delivered {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                delivered_at: Utc::now(),
                turn_id,
                reply_message_id,
            };
            (AgentDeliveryStatus::Delivered, event)
        }
        ClaimSettlement::Failed {
            error,
            retryable,
            next_attempt_at,
        } => {
            let event = AgentInboxEvent::Failed {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                failed_at: Utc::now(),
                error,
                retryable,
                next_attempt_at,
            };
            (AgentDeliveryStatus::Failed, event)
        }
    };
    let accepted_at = entry.accepted_at;
    let durability = authority.append(event)?;
    Ok(AgentDeliveryReceipt {
        message_id: claim.message.message_id.clone(),
        target: target.clone(),
        status,
        accepted_at,
        duplicate: false,
        durability: durability.into(),
    })
}

fn records_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Vec<AgentDeliveryRecord>, AgentRouterError> {
    let authority = authority_for(root, inboxes, target)?;
    authority.with_projection(|projection| {
        projection.validate(&authority.directory, target)?;
        Ok(projection
            .ordered(&authority.directory)?
            .into_iter()
            .map(FoldedDelivery::record)
            .collect())
    })
}

fn next_attempt_at_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Option<DateTime<Utc>>, AgentRouterError> {
    let authority = authority_for(root, inboxes, target)?;
    authority.with_projection(|projection| {
        Ok(projection
            .frontier_entry()
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
    echo_agent::utils::fs::atomic_write(path, &encoded).map_err(|source| AgentRouterError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FoldedDelivery {
    message: AgentMessage,
    accepted_at: DateTime<Utc>,
    status: AgentDeliveryStatus,
    attempt_id: Option<String>,
    attempt: u32,
    claimed_at: Option<DateTime<Utc>>,
    settled_at: Option<DateTime<Utc>>,
    turn_id: Option<String>,
    reply_message_id: Option<String>,
    error: Option<String>,
    next_attempt_at: Option<DateTime<Utc>>,
    terminal: bool,
    retained_bytes: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AgentInboxProjection {
    order: VecDeque<String>,
    frontier: VecDeque<String>,
    entries: HashMap<String, FoldedDelivery>,
    terminal_retained_bytes: usize,
    invalid: Option<String>,
    #[cfg(test)]
    #[serde(skip)]
    full_validation_count: std::sync::atomic::AtomicUsize,
}

impl EventReducer for AgentInboxProjection {
    type Event = AgentInboxEvent;

    fn apply(&mut self, event: &Self::Event) {
        if self.invalid.is_none()
            && let Err(error) = self.apply_checked(event)
        {
            self.invalid = Some(error);
        }
    }
}

impl AgentInboxProjection {
    fn validate(&self, path: &Path, target: &AgentAddress) -> Result<(), AgentRouterError> {
        #[cfg(test)]
        self.full_validation_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.ensure_incremental_valid(path)?;
        let mut saw_non_terminal = false;
        let mut terminal_count = 0_usize;
        for message_id in &self.order {
            let entry = self.entries.get(message_id).ok_or_else(|| {
                corrupt_event(
                    path,
                    format!("message {message_id} disappeared from its projection"),
                )
            })?;
            if &entry.message.to != target {
                return Err(corrupt_event(
                    path,
                    format!("message {message_id} targets a different Agent address"),
                ));
            }
            if entry.terminal {
                if saw_non_terminal {
                    return Err(corrupt_event(
                        path,
                        "terminal Agent inbox entry appears after the live frontier".to_string(),
                    ));
                }
                terminal_count = terminal_count.saturating_add(1);
            } else {
                saw_non_terminal = true;
            }
        }
        if self.entries.len() != self.order.len() {
            return Err(corrupt_event(
                path,
                "Agent inbox projection order and entry counts differ".to_string(),
            ));
        }
        let mut frontier_ids = HashSet::with_capacity(self.frontier.len());
        for message_id in &self.frontier {
            if !frontier_ids.insert(message_id) {
                return Err(corrupt_event(
                    path,
                    format!("frontier contains duplicate message {message_id}"),
                ));
            }
            let entry = self.entries.get(message_id).ok_or_else(|| {
                corrupt_event(path, format!("frontier message {message_id} is missing"))
            })?;
            if entry.terminal {
                return Err(corrupt_event(
                    path,
                    format!("terminal message {message_id} remains on the frontier"),
                ));
            }
        }
        if self
            .entries
            .values()
            .filter(|entry| !entry.terminal)
            .count()
            != self.frontier.len()
        {
            return Err(corrupt_event(
                path,
                "Agent inbox frontier omits a non-terminal message".to_string(),
            ));
        }
        if terminal_count > INBOX_TERMINAL_RETENTION {
            return Err(corrupt_event(
                path,
                "Agent inbox terminal retention exceeds its fixed bound".to_string(),
            ));
        }
        let terminal_bytes = self
            .entries
            .values()
            .filter(|entry| entry.terminal)
            .map(|entry| entry.retained_bytes)
            .fold(0_usize, usize::saturating_add);
        if terminal_bytes != self.terminal_retained_bytes {
            return Err(corrupt_event(
                path,
                "Agent inbox terminal byte accounting diverged".to_string(),
            ));
        }
        if terminal_bytes > INBOX_TERMINAL_RETENTION_BYTES {
            return Err(corrupt_event(
                path,
                "Agent inbox terminal byte retention exceeds its fixed bound".to_string(),
            ));
        }
        Ok(())
    }

    fn ensure_incremental_valid(&self, path: &Path) -> Result<(), AgentRouterError> {
        match &self.invalid {
            Some(error) => Err(corrupt_event(path, error.clone())),
            None => Ok(()),
        }
    }

    fn message(&self, message_id: &str) -> Option<&FoldedDelivery> {
        self.entries.get(message_id)
    }

    fn frontier_entry(&self) -> Option<&FoldedDelivery> {
        self.frontier
            .front()
            .and_then(|message_id| self.entries.get(message_id))
    }

    fn frontier_entries(&self) -> impl Iterator<Item = &FoldedDelivery> {
        self.frontier
            .iter()
            .filter_map(|message_id| self.entries.get(message_id))
    }

    fn ordered(&self, path: &Path) -> Result<Vec<FoldedDelivery>, AgentRouterError> {
        self.order
            .iter()
            .map(|message_id| {
                self.entries.get(message_id).cloned().ok_or_else(|| {
                    corrupt_event(
                        path,
                        format!("message {message_id} disappeared from its projection"),
                    )
                })
            })
            .collect()
    }

    fn apply_checked(&mut self, event: &AgentInboxEvent) -> Result<(), String> {
        match event {
            AgentInboxEvent::Accepted {
                message,
                accepted_at,
            } => {
                if self.entries.contains_key(&message.message_id) {
                    return Err(format!("duplicate acceptance for {}", message.message_id));
                }
                self.order.push_back(message.message_id.clone());
                self.frontier.push_back(message.message_id.clone());
                self.entries.insert(
                    message.message_id.clone(),
                    FoldedDelivery {
                        message: message.clone(),
                        accepted_at: *accepted_at,
                        status: AgentDeliveryStatus::Queued,
                        attempt_id: None,
                        attempt: 0,
                        claimed_at: None,
                        settled_at: None,
                        turn_id: None,
                        reply_message_id: None,
                        error: None,
                        next_attempt_at: None,
                        terminal: false,
                        retained_bytes: 0,
                    },
                );
            }
            AgentInboxEvent::Claimed {
                message_id,
                attempt_id,
                attempt,
                claimed_at,
            } => {
                let entry = projection_entry_mut(&mut self.entries, message_id)?;
                if entry.terminal {
                    return Err(format!("terminal message {message_id} was claimed again"));
                }
                entry.status = AgentDeliveryStatus::Claimed;
                entry.attempt_id = Some(attempt_id.clone());
                entry.attempt = *attempt;
                entry.claimed_at = Some(*claimed_at);
                entry.settled_at = None;
                entry.turn_id = None;
                entry.reply_message_id = None;
                entry.error = None;
                entry.next_attempt_at = None;
            }
            AgentInboxEvent::InjectionStarted {
                message_id,
                attempt_id,
                started_at,
                turn_id,
            } => {
                let entry =
                    projection_claimed_entry_mut(&mut self.entries, message_id, attempt_id)?;
                if entry.status != AgentDeliveryStatus::Claimed {
                    return Err(format!(
                        "delivery injection was started twice for {message_id}"
                    ));
                }
                entry.status = AgentDeliveryStatus::InjectionStarted;
                entry.settled_at = Some(*started_at);
                entry.turn_id = Some(turn_id.clone());
            }
            AgentInboxEvent::Injected {
                message_id,
                attempt_id,
                injected_at,
                turn_id,
            } => {
                let entry =
                    projection_claimed_entry_mut(&mut self.entries, message_id, attempt_id)?;
                if entry.status != AgentDeliveryStatus::InjectionStarted {
                    return Err(format!(
                        "delivery was marked injected without a started fact for {message_id}"
                    ));
                }
                if entry.turn_id.as_deref() != Some(turn_id) {
                    return Err(format!("delivery injected turn changed for {message_id}"));
                }
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
                let entry =
                    projection_claimed_entry_mut(&mut self.entries, message_id, attempt_id)?;
                entry.status = AgentDeliveryStatus::Queued;
                entry.settled_at = Some(*deferred_at);
                entry.turn_id = None;
                entry.next_attempt_at = *next_attempt_at;
            }
            AgentInboxEvent::Delivered {
                message_id,
                attempt_id,
                delivered_at,
                turn_id,
                reply_message_id,
            } => {
                {
                    let entry =
                        projection_claimed_entry_mut(&mut self.entries, message_id, attempt_id)?;
                    entry.status = AgentDeliveryStatus::Delivered;
                    entry.settled_at = Some(*delivered_at);
                    entry.turn_id = Some(turn_id.clone());
                    entry.reply_message_id = reply_message_id.clone();
                    entry.terminal = true;
                    entry.next_attempt_at = None;
                }
                self.retain_terminal(message_id)?;
            }
            AgentInboxEvent::Failed {
                message_id,
                attempt_id,
                failed_at,
                error,
                retryable,
                next_attempt_at,
            } => {
                let terminal = !retryable;
                {
                    let entry =
                        projection_claimed_entry_mut(&mut self.entries, message_id, attempt_id)?;
                    entry.status = AgentDeliveryStatus::Failed;
                    entry.settled_at = Some(*failed_at);
                    entry.error = Some(error.clone());
                    entry.terminal = terminal;
                    entry.next_attempt_at = *next_attempt_at;
                }
                if terminal {
                    self.retain_terminal(message_id)?;
                }
            }
        }
        Ok(())
    }

    fn retire_frontier(&mut self, message_id: &str) -> Result<(), String> {
        if self.frontier.front().map(String::as_str) != Some(message_id) {
            return Err(format!(
                "terminal delivery {message_id} is not the FIFO frontier owner"
            ));
        }
        self.frontier.pop_front();
        Ok(())
    }

    fn retain_terminal(&mut self, message_id: &str) -> Result<(), String> {
        let retained_bytes = {
            let entry = self
                .entries
                .get_mut(message_id)
                .ok_or_else(|| format!("terminal message {message_id} is missing"))?;
            let payload = serde_json::to_vec(&entry)
                .map_err(|error| format!("terminal retention encoding failed: {error}"))?;
            let retained_bytes = payload
                .len()
                .saturating_add(message_id.len().saturating_mul(3))
                .saturating_add(128);
            entry.retained_bytes = retained_bytes;
            retained_bytes
        };
        self.terminal_retained_bytes = self.terminal_retained_bytes.saturating_add(retained_bytes);
        self.retire_frontier(message_id)?;
        self.trim_terminal_history()
    }

    fn trim_terminal_history(&mut self) -> Result<(), String> {
        while self.entries.len().saturating_sub(self.frontier.len()) > INBOX_TERMINAL_RETENTION
            || self.terminal_retained_bytes > INBOX_TERMINAL_RETENTION_BYTES
        {
            let message_id = self
                .order
                .pop_front()
                .ok_or_else(|| "Agent inbox terminal retention lost its order".to_string())?;
            let terminal = self
                .entries
                .get(&message_id)
                .is_some_and(|entry| entry.terminal);
            if !terminal {
                return Err(format!(
                    "Agent inbox attempted to evict live frontier message {message_id}"
                ));
            }
            let removed = self
                .entries
                .remove(&message_id)
                .ok_or_else(|| format!("terminal message {message_id} disappeared during trim"))?;
            self.terminal_retained_bytes = self
                .terminal_retained_bytes
                .saturating_sub(removed.retained_bytes);
        }
        Ok(())
    }
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

fn projection_entry_mut<'a>(
    entries: &'a mut HashMap<String, FoldedDelivery>,
    message_id: &str,
) -> Result<&'a mut FoldedDelivery, String> {
    entries
        .get_mut(message_id)
        .ok_or_else(|| format!("delivery event references unknown message {message_id}"))
}

fn projection_claimed_entry_mut<'a>(
    entries: &'a mut HashMap<String, FoldedDelivery>,
    message_id: &str,
    attempt_id: &str,
) -> Result<&'a mut FoldedDelivery, String> {
    let entry = projection_entry_mut(entries, message_id)?;
    if !matches!(
        entry.status,
        AgentDeliveryStatus::Claimed
            | AgentDeliveryStatus::InjectionStarted
            | AgentDeliveryStatus::Injected
    ) || entry.attempt_id.as_deref() != Some(attempt_id)
    {
        return Err(format!(
            "delivery event has stale claim {attempt_id} for {message_id}"
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

fn inbox_dir(root: &Path, target: &AgentAddress) -> PathBuf {
    root.join("inboxes")
        .join(stable_segment(target.workspace_id.as_str()))
        .join(stable_segment(&target.conversation_id))
}

fn stable_segment(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn journal_error(path: &Path, error: echo_agent::error::ReactError) -> AgentRouterError {
    AgentRouterError::Corrupt {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
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

    fn no_delivery_recovery() -> Arc<dyn Fn(AgentAddress) + Send + Sync> {
        Arc::new(|_| {})
    }

    async fn mark_delivered(
        router: &AgentRouter,
        claim: &AgentDeliveryClaim,
        turn_id: &str,
    ) -> Result<(), String> {
        router
            .begin_injection(claim, turn_id)
            .await
            .map_err(|error| error.to_string())?;
        router
            .injected(claim, turn_id)
            .await
            .map_err(|error| error.to_string())?;
        router
            .delivered(claim, turn_id, None)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn delivery_supervisor_closes_admission_before_join() -> Result<(), String> {
        let supervisor = AgentDeliverySupervisor::default();
        let cancel = supervisor.cancellation_token();
        let started = Arc::new(tokio::sync::Notify::new());
        let task_started = Arc::clone(&started);
        supervisor
            .supervise(
                address(),
                no_delivery_recovery(),
                move |_cycle| async move {
                    task_started.notify_one();
                    cancel.cancelled().await;
                },
            )
            .map_err(|error| error.to_string())?;
        started.notified().await;

        supervisor
            .close_admission_and_cancel()
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            supervisor.supervise(address(), no_delivery_recovery(), |_cycle| {
                std::future::pending()
            },),
            Err(AgentRouterError::ShuttingDown)
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), supervisor.join())
            .await
            .map_err(|_| "delivery supervisor join ignored cancellation".to_string())?
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn target_retirement_linearizes_admission_and_waits_for_active_driver()
    -> Result<(), String> {
        let supervisor = Arc::new(AgentDeliverySupervisor::default());
        let target = address();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let task_started = Arc::clone(&started);
        let task_release = Arc::clone(&release);
        supervisor
            .supervise(
                target.clone(),
                no_delivery_recovery(),
                move |_cycle| async move {
                    task_started.notify_one();
                    task_release.notified().await;
                },
            )
            .map_err(|error| error.to_string())?;
        started.notified().await;

        let retiring_supervisor = Arc::clone(&supervisor);
        let retiring_target = target.clone();
        let retirement =
            tokio::spawn(async move { retiring_supervisor.retire_target(retiring_target).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !supervisor.is_retiring_target(&target) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "target retirement did not close admission".to_string())?;
        assert!(matches!(
            supervisor.supervise(target.clone(), no_delivery_recovery(), |_cycle| async {}),
            Err(AgentRouterError::Retiring { .. })
        ));
        release.notify_one();
        let guard = retirement
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            supervisor.supervise(target.clone(), no_delivery_recovery(), |_cycle| async {}),
            Err(AgentRouterError::Retiring { .. })
        ));
        drop(guard);
        assert!(
            supervisor
                .supervise(target, no_delivery_recovery(), |cycle| async move {
                    let _ = cycle.complete();
                })
                .map_err(|error| error.to_string())?
        );
        supervisor
            .shutdown()
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn workspace_retirement_blocks_only_that_workspace_delivery_admission()
    -> Result<(), String> {
        let supervisor = AgentDeliverySupervisor::default();
        let target = address();
        let guard = supervisor
            .retire_workspace(target.workspace_id.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            supervisor.supervise(target, no_delivery_recovery(), |_cycle| async {}),
            Err(AgentRouterError::Retiring { .. })
        ));
        let other = AgentAddress::new(WorkspaceId::from_name("other"), "conversation");
        assert!(
            supervisor
                .supervise(other, no_delivery_recovery(), |cycle| async move {
                    let _ = cycle.complete();
                })
                .map_err(|error| error.to_string())?
        );
        drop(guard);
        supervisor
            .shutdown()
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn router_two_phase_retirement_closes_mutation_before_purge() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let router = AgentRouter::new(temp.path().to_path_buf());
        router
            .enqueue(AgentMessage::user_text(
                None,
                target.clone(),
                "accepted before retirement",
            ))
            .await
            .map_err(|error| error.to_string())?;
        let guard = router
            .begin_target_retirement(target.clone())
            .map_err(|error| error.to_string())?;
        assert!(inbox_dir(temp.path(), &target).exists());
        assert!(matches!(
            router
                .enqueue(AgentMessage::user_text(
                    None,
                    target.clone(),
                    "rejected after retirement cut",
                ))
                .await,
            Err(AgentRouterError::Retiring { .. })
        ));
        assert!(matches!(
            router.claim_next(&target).await,
            Err(AgentRouterError::Retiring { .. })
        ));
        guard.purge().await.map_err(|error| error.to_string())?;
        assert!(!inbox_dir(temp.path(), &target).exists());
        drop(guard);
        assert!(
            router
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn delivery_driver_panic_clears_active_and_reaches_shutdown_receipt() -> Result<(), String>
    {
        let supervisor = AgentDeliverySupervisor::default();
        let target = address();
        let workspace = target.workspace_id.clone();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let task_started = Arc::clone(&started);
        let task_release = Arc::clone(&release);
        supervisor
            .supervise(
                target.clone(),
                no_delivery_recovery(),
                move |_cycle| async move {
                    task_started.notify_one();
                    task_release.notified().await;
                    let should_complete = std::hint::black_box(false);
                    assert!(should_complete, "injected delivery driver panic");
                },
            )
            .map_err(|error| error.to_string())?;
        started.notified().await;
        release.notify_one();

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while supervisor.has_active_workspace(&workspace) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "panicked delivery driver retained active target".to_string())?;
        assert!(!supervisor.has_active_workspace(&workspace));

        let error = supervisor
            .join()
            .await
            .err()
            .ok_or_else(|| "delivery driver panic was not reported".to_string())?;
        assert!(error.to_string().contains(target.conversation_id.as_str()));
        Ok(())
    }

    #[tokio::test]
    async fn dirty_delivery_is_restarted_after_driver_panic() -> Result<(), String> {
        let supervisor = Arc::new(AgentDeliverySupervisor::default());
        let target = address();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let recovered = Arc::new(tokio::sync::Notify::new());
        let weak_supervisor = Arc::downgrade(&supervisor);
        let recovered_callback = Arc::clone(&recovered);
        let recover: Arc<dyn Fn(AgentAddress) + Send + Sync> = Arc::new(move |target| {
            let Some(supervisor) = weak_supervisor.upgrade() else {
                return;
            };
            let recovered = Arc::clone(&recovered_callback);
            let _ = supervisor.supervise(target, no_delivery_recovery(), move |cycle| async move {
                let _ = cycle.complete();
                recovered.notify_one();
            });
        });
        let task_started = Arc::clone(&started);
        let task_release = Arc::clone(&release);
        supervisor
            .supervise(target.clone(), recover, move |_cycle| async move {
                task_started.notify_one();
                task_release.notified().await;
                let should_complete = std::hint::black_box(false);
                assert!(should_complete, "injected dirty delivery panic");
            })
            .map_err(|error| error.to_string())?;
        started.notified().await;
        assert!(
            !supervisor
                .supervise(target, no_delivery_recovery(), |_cycle| {
                    std::future::pending()
                },)
                .map_err(|error| error.to_string())?,
            "dirty wake created a duplicate delivery owner"
        );
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), recovered.notified())
            .await
            .map_err(|_| "dirty delivery was not restarted after panic".to_string())?;

        let error = supervisor
            .shutdown()
            .await
            .err()
            .ok_or_else(|| "recovered driver panic was not retained in receipt".to_string())?;
        assert!(error.to_string().contains("panicked"));
        Ok(())
    }

    #[tokio::test]
    async fn delayed_old_driver_drop_cannot_clear_replacement_generation() -> Result<(), String> {
        let supervisor = Arc::new(AgentDeliverySupervisor::default());
        let target = address();
        let a_removed = Arc::new(tokio::sync::Notify::new());
        let allow_a_drop = Arc::new(tokio::sync::Notify::new());
        let a_removed_task = Arc::clone(&a_removed);
        let allow_a_drop_task = Arc::clone(&allow_a_drop);
        supervisor
            .supervise(
                target.clone(),
                no_delivery_recovery(),
                move |cycle| async move {
                    let repeated = cycle.complete().unwrap_or(false);
                    assert!(!repeated, "driver A unexpectedly retained its generation");
                    a_removed_task.notify_one();
                    allow_a_drop_task.notified().await;
                },
            )
            .map_err(|error| error.to_string())?;
        a_removed.notified().await;

        let allow_b_cycles = Arc::new(tokio::sync::Notify::new());
        let b_completed = Arc::new(tokio::sync::Notify::new());
        let allow_b_cycles_task = Arc::clone(&allow_b_cycles);
        let b_completed_task = Arc::clone(&b_completed);
        let inserted = supervisor
            .supervise(
                target.clone(),
                no_delivery_recovery(),
                move |cycle| async move {
                    allow_b_cycles_task.notified().await;
                    let repeated = cycle.complete().unwrap_or(false);
                    assert!(repeated, "driver B lost its dirty notification");
                    let repeated = cycle.complete().unwrap_or(true);
                    assert!(!repeated, "driver B did not release its generation");
                    b_completed_task.notify_one();
                },
            )
            .map_err(|error| error.to_string())?;
        assert!(inserted, "driver B did not acquire the released target");
        assert!(
            !supervisor
                .supervise(target.clone(), no_delivery_recovery(), |_cycle| {
                    std::future::pending()
                },)
                .map_err(|error| error.to_string())?,
            "a second B wake created a duplicate owner"
        );

        allow_a_drop.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let _ = supervisor.supervise(target.clone(), no_delivery_recovery(), |_cycle| {
                    std::future::pending()
                });
                let old_driver_collected = supervisor
                    .state
                    .lock()
                    .map(|state| state.driver_targets.len() == 1)
                    .unwrap_or(false);
                if old_driver_collected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "driver A did not reach its delayed Drop barrier".to_string())?;

        {
            let state = supervisor
                .state
                .lock()
                .map_err(|_| "delivery supervisor state is unavailable".to_string())?;
            let b_generation = state
                .active
                .get(&target)
                .copied()
                .ok_or_else(|| "driver A Drop cleared driver B active owner".to_string())?;
            assert_eq!(
                state.dirty.get(&target),
                Some(&b_generation),
                "driver A Drop cleared driver B dirty notification"
            );
        }

        allow_b_cycles.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), b_completed.notified())
            .await
            .map_err(|_| "driver B did not complete both owned cycles".to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while supervisor.has_active_workspace(&target.workspace_id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "driver B retained its active generation".to_string())?;
        supervisor
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn drain_inbox(root: PathBuf, target: AgentAddress) -> Result<usize, String> {
        let router = AgentRouter::new(root);
        let mut delivered = 0usize;
        while let Some(claim) = router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
        {
            mark_delivered(&router, &claim, &claim.message.delivery_turn_id()).await?;
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
        assert_eq!(first.durability, AgentDeliveryDurability::Confirmed);
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        let duplicate = restarted
            .enqueue(message.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.durability, AgentDeliveryDurability::Unconfirmed);
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
        let path = inbox_dir(temp.path(), &target).join("journal");
        let journal =
            SegmentedFileEventJournal::open(&path, INBOX_SEGMENT_BYTES, FileDurability::SyncData)
                .map_err(|error| error.to_string())?;
        journal
            .append(AgentInboxEvent::Accepted {
                message: AgentMessage::user_text(None, target.clone(), "persisted"),
                accepted_at: Utc::now(),
            })
            .map_err(|error| error.to_string())?;
        let segment = journal
            .segments()
            .into_iter()
            .find(|segment| segment.active)
            .map(|segment| segment.path)
            .ok_or_else(|| "active Agent inbox segment missing".to_string())?;
        drop(journal);
        use std::io::Write as _;
        let mut file = OpenOptions::new()
            .append(true)
            .open(&segment)
            .map_err(|error| error.to_string())?;
        file.write_all(b"{broken}\n")
            .map_err(|error| error.to_string())?;
        file.sync_data().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());

        assert!(matches!(
            router.pending(&target).await,
            Err(AgentRouterError::Corrupt { .. })
        ));
        assert!(
            std::fs::read_to_string(segment)
                .map_err(|error| error.to_string())?
                .ends_with("{broken}\n")
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
        mark_delivered(&router, &retry, "turn-first").await?;
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
        mark_delivered(&restarted, &recovered, "recovered").await?;
        let duplicate = restarted
            .enqueue(recovered.message)
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.status, AgentDeliveryStatus::Delivered);
        Ok(())
    }

    #[tokio::test]
    async fn restart_never_reclaims_an_injected_attempt() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut message = AgentMessage::user_text(None, target.clone(), "do not replay");
        message.message_id = "injected-before-restart".to_string();
        router
            .enqueue(message)
            .await
            .map_err(|error| error.to_string())?;
        let claim = router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "claim missing".to_string())?;
        router
            .begin_injection(&claim, "turn-before-restart")
            .await
            .map_err(|error| error.to_string())?;
        router
            .injected(&claim, "turn-before-restart")
            .await
            .map_err(|error| error.to_string())?;
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        assert!(
            restarted
                .claim_next(&target)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        let recovered = restarted
            .in_flight_claim(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "injected recovery identity missing".to_string())?;
        assert_eq!(recovered.claim.attempt_id, claim.attempt_id);
        assert_eq!(recovered.claim.attempt, claim.attempt);
        assert_eq!(recovered.status, AgentDeliveryStatus::Injected);
        assert_eq!(recovered.turn_id, "turn-before-restart");
        restarted
            .failed(&recovered.claim, "outcome indeterminate", false)
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            restarted
                .in_flight_claim(&target)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert_eq!(
            restarted
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .first()
                .map(|record| (record.status, record.attempt)),
            Some((AgentDeliveryStatus::Failed, 1))
        );
        Ok(())
    }

    #[tokio::test]
    async fn injection_started_crash_preserves_attempt_and_actual_turn_without_replay()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut message = AgentMessage::user_text(None, target.clone(), "started crash");
        message.message_id = "started-before-crash".to_string();
        router
            .enqueue(message)
            .await
            .map_err(|error| error.to_string())?;
        let claim = router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "claim missing".to_string())?;
        router
            .begin_injection(&claim, "actual-active-turn")
            .await
            .map_err(|error| error.to_string())?;
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        assert!(
            restarted
                .claim_next(&target)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        let in_flight = restarted
            .in_flight_claim(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "started recovery missing".to_string())?;
        assert_eq!(in_flight.claim.attempt_id, claim.attempt_id);
        assert_eq!(in_flight.status, AgentDeliveryStatus::InjectionStarted);
        assert_eq!(in_flight.turn_id, "actual-active-turn");
        restarted
            .failed(&in_flight.claim, "outcome unknown", false)
            .await
            .map_err(|error| error.to_string())?;
        let record = restarted
            .records(&target)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "terminal record missing".to_string())?;
        assert_eq!(record.status, AgentDeliveryStatus::Failed);
        assert_eq!(record.attempt, 1);
        assert_eq!(record.turn_id.as_deref(), Some("actual-active-turn"));
        Ok(())
    }

    #[tokio::test]
    async fn checkpointed_inbox_restarts_from_projection_and_retirement_forgets_history()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let router = AgentRouter::new(temp.path().to_path_buf());
        for index in 0..70 {
            let mut message = AgentMessage::user_text(
                None,
                target.clone(),
                format!("checkpointed message {index}"),
            );
            message.message_id = format!("checkpointed-{index}");
            router
                .enqueue(message)
                .await
                .map_err(|error| error.to_string())?;
        }
        let checkpoint = inbox_dir(temp.path(), &target).join("projection.checkpoint.json");
        let frame = FileCheckpointStore::<AgentInboxProjection>::open(&checkpoint)
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Agent inbox checkpoint was not compounded".to_string())?;
        assert_eq!(frame.sequence, INBOX_CHECKPOINT_EVERY);
        assert_eq!(frame.state.order.len(), INBOX_CHECKPOINT_EVERY as usize);
        assert!(
            !inbox_dir(temp.path(), &target)
                .join("events.jsonl")
                .exists()
        );
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        assert_eq!(
            restarted
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .len(),
            70
        );
        let retirement = restarted
            .retire_target(target.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            restarted.records(&target).await,
            Err(AgentRouterError::Retiring { .. })
        ));
        drop(retirement);
        assert!(
            restarted
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        let mut rebuilt = AgentMessage::user_text(None, target.clone(), "fresh generation");
        rebuilt.message_id = "fresh-generation".to_string();
        restarted
            .enqueue(rebuilt)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            restarted
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        Ok(())
    }

    fn apply_terminal_projection_lifecycle(
        projection: &mut AgentInboxProjection,
        target: &AgentAddress,
        index: usize,
        timestamp: DateTime<Utc>,
        text: &str,
    ) -> Result<(), String> {
        let message_id = format!("scale-message-{index}");
        let attempt_id = format!("scale-attempt-{index}");
        let turn_id = format!("scale-turn-{index}");
        let mut message = AgentMessage::user_text(None, target.clone(), text);
        message.message_id = message_id.clone();
        projection.apply_checked(&AgentInboxEvent::Accepted {
            message,
            accepted_at: timestamp,
        })?;
        projection.apply_checked(&AgentInboxEvent::Claimed {
            message_id: message_id.clone(),
            attempt_id: attempt_id.clone(),
            attempt: 1,
            claimed_at: timestamp,
        })?;
        projection.apply_checked(&AgentInboxEvent::InjectionStarted {
            message_id: message_id.clone(),
            attempt_id: attempt_id.clone(),
            started_at: timestamp,
            turn_id: turn_id.clone(),
        })?;
        projection.apply_checked(&AgentInboxEvent::Injected {
            message_id: message_id.clone(),
            attempt_id: attempt_id.clone(),
            injected_at: timestamp,
            turn_id: turn_id.clone(),
        })?;
        projection.apply_checked(&AgentInboxEvent::Delivered {
            message_id,
            attempt_id,
            delivered_at: timestamp,
            turn_id,
            reply_message_id: None,
        })
    }

    fn measure_terminal_projection(
        event_count: usize,
    ) -> Result<(std::time::Duration, std::time::Duration, usize), String> {
        let target = address();
        let mut projection = AgentInboxProjection::default();
        let timestamp = Utc::now();
        let halfway = event_count / 2;
        let started = std::time::Instant::now();
        let mut midpoint = started;
        for index in 0..event_count {
            if index == halfway {
                midpoint = std::time::Instant::now();
            }
            apply_terminal_projection_lifecycle(
                &mut projection,
                &target,
                index,
                timestamp,
                "scale terminal",
            )?;
        }
        let finished = std::time::Instant::now();
        assert_eq!(projection.order.len(), INBOX_TERMINAL_RETENTION);
        assert_eq!(projection.entries.len(), INBOX_TERMINAL_RETENTION);
        assert!(projection.frontier.is_empty());
        assert_eq!(
            projection
                .full_validation_count
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "hot reducer mutation unexpectedly ran a full projection validation"
        );
        projection
            .validate(Path::new("scale-projection"), &target)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            projection
                .full_validation_count
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        let checkpoint_bytes = serde_json::to_vec(&projection)
            .map_err(|error| format!("projection checkpoint serialization failed: {error}"))?
            .len();
        Ok((
            midpoint.saturating_duration_since(started),
            finished.saturating_duration_since(midpoint),
            checkpoint_bytes,
        ))
    }

    #[test]
    fn terminal_projection_is_bounded_at_10k_and_100k_without_hot_full_validation()
    -> Result<(), String> {
        let (_ten_first, _ten_second, ten_checkpoint) = measure_terminal_projection(10_000)?;
        let (hundred_first, hundred_second, hundred_checkpoint) =
            measure_terminal_projection(100_000)?;
        let second_half_budget = hundred_first
            .saturating_mul(4)
            .saturating_add(std::time::Duration::from_millis(250));
        assert!(
            hundred_second <= second_half_budget,
            "100k terminal projection second half regressed: first={hundred_first:?}, second={hundred_second:?}, budget={second_half_budget:?}"
        );
        assert!(
            hundred_checkpoint <= ten_checkpoint.saturating_add(16 * 1024),
            "bounded checkpoint grew with terminal history: 10k={ten_checkpoint}, 100k={hundred_checkpoint}"
        );
        assert!(hundred_checkpoint <= 512 * 1024);
        Ok(())
    }

    #[test]
    fn near_max_terminal_payloads_obey_the_absolute_checkpoint_byte_budget() -> Result<(), String> {
        let target = address();
        let timestamp = Utc::now();
        let payload = "x".repeat(MAX_TEXT_CHARS);
        let mut projection = AgentInboxProjection::default();
        for index in 0..8 {
            apply_terminal_projection_lifecycle(
                &mut projection,
                &target,
                index,
                timestamp,
                &payload,
            )?;
        }
        projection
            .validate(Path::new("large-terminal-projection"), &target)
            .map_err(|error| error.to_string())?;
        assert!(projection.order.len() < 8);
        assert!(projection.terminal_retained_bytes <= INBOX_TERMINAL_RETENTION_BYTES);
        let checkpoint = serde_json::to_vec(&projection)
            .map_err(|error| format!("large checkpoint serialization failed: {error}"))?;
        assert!(
            checkpoint.len() <= 512 * 1024,
            "large terminal checkpoint exceeded absolute budget: {} bytes",
            checkpoint.len()
        );

        let mut oversized = AgentInboxProjection::default();
        let unicode_payload = "界".repeat(MAX_TEXT_CHARS);
        apply_terminal_projection_lifecycle(
            &mut oversized,
            &target,
            0,
            timestamp,
            &unicode_payload,
        )?;
        assert!(oversized.order.is_empty());
        assert_eq!(oversized.terminal_retained_bytes, 0);
        Ok(())
    }

    #[tokio::test]
    async fn hot_agent_inbox_mutations_do_not_run_full_projection_validation() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let router = AgentRouter::new(temp.path().to_path_buf());
        let message = AgentMessage::user_text(None, target.clone(), "validation counter");
        router
            .enqueue(message)
            .await
            .map_err(|error| error.to_string())?;
        let claim = router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "validation-counter claim is missing".to_string())?;
        router
            .begin_injection(&claim, "validation-turn")
            .await
            .map_err(|error| error.to_string())?;
        router
            .injected(&claim, "validation-turn")
            .await
            .map_err(|error| error.to_string())?;
        router
            .delivered(&claim, "validation-turn", None)
            .await
            .map_err(|error| error.to_string())?;
        let authority = authority_for(temp.path(), &router.inboxes, &target)
            .map_err(|error| error.to_string())?;
        let validations = authority
            .with_projection(|projection| {
                Ok(projection
                    .full_validation_count
                    .load(std::sync::atomic::Ordering::Relaxed))
            })
            .map_err(|error| error.to_string())?;
        assert_eq!(validations, 1, "hot append reran full validation");
        Ok(())
    }

    #[tokio::test]
    async fn workspace_retirement_forgets_only_that_workspace() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let first = AgentAddress::new(WorkspaceId::from_name("retired"), "first");
        let second = AgentAddress::new(WorkspaceId::from_name("retired"), "second");
        let retained = AgentAddress::new(WorkspaceId::from_name("retained"), "third");
        for target in [&first, &second, &retained] {
            router
                .enqueue(AgentMessage::user_text(
                    None,
                    target.clone(),
                    "workspace retirement fixture",
                ))
                .await
                .map_err(|error| error.to_string())?;
        }
        let retirement = router
            .retire_workspace(first.workspace_id.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            router.records(&first).await,
            Err(AgentRouterError::Retiring { .. })
        ));
        assert_eq!(
            router
                .records(&retained)
                .await
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        drop(retirement);
        assert!(
            router
                .records(&first)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert!(
            router
                .records(&second)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert_eq!(
            router
                .records(&retained)
                .await
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
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
