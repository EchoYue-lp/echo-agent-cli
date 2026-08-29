// Application-owned ordered event journal for ordinary chat turns.
//
// The framework owns physical sequencing, segmentation, integrity, recovery,
// durability and pruning. EKO owns stream identity, product retention pins and
// projections for GUI, TUI, CLI, channels and boot recovery.

use crate::chat_driver::ChatDriverEvent;
use crate::conversation_input::{
    ConversationInputAddress, ConversationInputAttempt, ConversationInputError,
    ConversationInputFact, ConversationInputFrontier, ConversationInputIdentity,
    ConversationInputOutcome, ConversationInputPayload, ConversationInputPhase,
    ConversationInputProjection, ConversationInputReceipt,
};
use crate::tool_execution::ToolExecutionRepository;
use crate::tool_execution_projection::ToolExecutionProjector;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use echo_agent::state::journal::{
    EventJournal, JournalDurabilityStatus, JournalPhysicalCleanupStatus, JournalPruneCommitStatus,
    JournalRecord, SegmentedFileEventJournal,
};
use echo_agent::utils::fs::FileDurability;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

pub const CHAT_EVENT_SCHEMA_VERSION: u16 = 2;
const REPLAY_BATCH_SIZE: usize = 4096;
const MAX_CACHED_STREAMS: usize = 128;
const PROCESS_CHAT_EVENT_IO_LIMIT: usize = 8;
static PROCESS_CHAT_EVENT_IO: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(PROCESS_CHAT_EVENT_IO_LIMIT)));
const MAX_REGISTRY_ENTRIES_BEFORE_PRUNE: usize = MAX_CACHED_STREAMS * 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatEventRetention {
    pub segment_rollover_bytes: u64,
    pub max_segments: usize,
    pub max_replay_events: usize,
}

impl Default for ChatEventRetention {
    fn default() -> Self {
        Self {
            segment_rollover_bytes: 1024 * 1024,
            max_segments: 8,
            max_replay_events: 4096,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatEventEnvelope {
    pub schema_version: u16,
    pub event_id: String,
    pub content_hash: String,
    pub sequence: u64,
    pub stream_id: String,
    pub workspace_id: String,
    pub conversation_id: Option<String>,
    pub root_turn_id: String,
    pub turn_id: String,
    pub message_id: String,
    pub timestamp: DateTime<Utc>,
    pub payload: ChatDriverEvent,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatEventReplay {
    pub events: Vec<ChatEventEnvelope>,
    pub retained_earliest_cursor: Option<u64>,
    pub returned_earliest_cursor: Option<u64>,
    pub latest_cursor: u64,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ChatEventLogError {
    #[error("chat event identity is invalid: {0}")]
    InvalidIdentity(String),
    #[error("chat event payload is invalid: {0}")]
    InvalidEvent(String),
    #[error("chat event log I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("chat event log is corrupt at {path}: {message}")]
    Corrupt { path: PathBuf, message: String },
    #[error("chat event serialization failed: {0}")]
    Serialization(String),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedChatEvent {
    schema_version: u16,
    stream_id: String,
    workspace_id: String,
    conversation_id: Option<String>,
    root_turn_id: String,
    turn_id: String,
    message_id: String,
    timestamp: DateTime<Utc>,
    payload: ChatDriverEvent,
}

type StreamJournal = SegmentedFileEventJournal<PersistedChatEvent>;
type StartedAwaiterDelivery = (
    String,
    Option<String>,
    String,
    crate::tasks::task_runtime::command_cells::AwaiterResultAcknowledgement,
);

#[derive(Debug, Default)]
struct RetentionPins {
    cursor: u64,
    pending_awaiters: HashMap<String, u64>,
    started_awaiters: HashMap<
        String,
        (
            u64,
            crate::tasks::task_runtime::command_cells::AwaiterResultAcknowledgement,
        ),
    >,
    active_cells: HashMap<String, u64>,
    conversation_inputs: HashMap<String, FoldedConversationInput>,
    queue_order: Vec<String>,
    queue_revision: u64,
    awaiter_facts: HashMap<String, u64>,
    earliest: Option<u64>,
    #[cfg(test)]
    recovered_records: usize,
}

#[derive(Debug, Clone)]
struct FoldedConversationInput {
    projection: ConversationInputProjection,
    first_sequence: u64,
    last_sequence: u64,
    terminal_fact_self_contained: bool,
}

#[derive(Debug)]
struct StreamAuthority {
    expected_stream_id: String,
    retention: ChatEventRetention,
    journal: StreamJournal,
    pins: RetentionPins,
    barrier_pending: bool,
}

type StreamAuthorityCell = Mutex<Option<StreamAuthority>>;
type CachedStreamJournal = Arc<StreamAuthorityCell>;
type StreamAuthorityRegistry = HashMap<PathBuf, Weak<StreamAuthorityCell>>;

fn stream_authority_registry() -> &'static Mutex<StreamAuthorityRegistry> {
    static REGISTRY: OnceLock<Mutex<StreamAuthorityRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct ChatEventLog {
    root: PathBuf,
    retention: ChatEventRetention,
    streams: DashMap<String, CachedStreamJournal>,
    stream_access: Mutex<VecDeque<String>>,
    #[cfg(test)]
    deletion_pause: Option<Arc<(std::sync::Barrier, std::sync::Barrier)>>,
    #[cfg(test)]
    orphan_recovery_pause: Option<Arc<(std::sync::Barrier, std::sync::Barrier)>>,
}

impl std::fmt::Debug for ChatEventLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatEventLog")
            .field("root", &self.root)
            .field("retention", &self.retention)
            .field("streams", &self.streams)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatSurface {
    Gui,
    Tui,
    Cli,
    Channel,
    BootRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatDeliveryGuarantee {
    BestEffort,
    JournaledWithSemanticSafePoints,
}

pub struct JournaledChatSink {
    inner: Arc<dyn crate::chat_driver::ChatSink>,
    log: Arc<ChatEventLog>,
    tool_execution_projector: Arc<ToolExecutionProjector>,
    surface: ChatSurface,
    workspace_id: String,
    conversation_id: Option<String>,
    turn_id: String,
}

impl JournaledChatSink {
    fn wrap(
        inner: Arc<dyn crate::chat_driver::ChatSink>,
        log: Arc<ChatEventLog>,
        tool_execution_projector: Arc<ToolExecutionProjector>,
        surface: ChatSurface,
        workspace_id: impl Into<String>,
        conversation_id: Option<String>,
        turn_id: impl Into<String>,
    ) -> Arc<dyn crate::chat_driver::ChatSink> {
        Arc::new(Self {
            inner,
            log,
            tool_execution_projector,
            surface,
            workspace_id: workspace_id.into(),
            conversation_id,
            turn_id: turn_id.into(),
        })
    }
}

pub fn bind_surface_chat_sink(
    surface: ChatSurface,
    inner: Arc<dyn crate::chat_driver::ChatSink>,
    log: Arc<ChatEventLog>,
    tool_executions: Arc<ToolExecutionRepository>,
    workspace_id: impl Into<String>,
    conversation_id: Option<String>,
    turn_id: impl Into<String>,
) -> Arc<dyn crate::chat_driver::ChatSink> {
    JournaledChatSink::wrap(
        inner,
        log,
        Arc::new(ToolExecutionProjector::new(tool_executions, None)),
        surface,
        workspace_id,
        conversation_id,
        turn_id,
    )
}

struct JournalOnlySink;

impl crate::chat_driver::ChatSink for JournalOnlySink {
    fn on_event(&self, _event: ChatDriverEvent) -> bool {
        true
    }
}

pub fn bind_boot_recovery_chat_sink(
    log: Arc<ChatEventLog>,
    tool_executions: Arc<ToolExecutionRepository>,
    workspace_id: impl Into<String>,
    conversation_id: String,
    root_turn_id: impl Into<String>,
) -> Arc<dyn crate::chat_driver::ChatSink> {
    bind_surface_chat_sink(
        ChatSurface::BootRecovery,
        Arc::new(JournalOnlySink),
        log,
        tool_executions,
        workspace_id,
        Some(conversation_id),
        root_turn_id,
    )
}

impl crate::chat_driver::ChatSink for JournaledChatSink {
    fn on_event(&self, event: ChatDriverEvent) -> bool {
        match self.log.append(
            &self.workspace_id,
            self.conversation_id.as_deref(),
            &self.turn_id,
            event,
        ) {
            Ok(envelope) => match self.tool_execution_projector.project_envelope(&envelope) {
                Ok(updates) => {
                    for update in &updates {
                        if !self.inner.on_tool_execution_projection(update) {
                            tracing::error!(surface = ?self.surface, "failed to deliver persisted tool-execution projection; closing surface stream");
                            return false;
                        }
                    }
                    self.inner.on_journaled_event(envelope)
                }
                Err(error) => {
                    tracing::error!(%error, surface = ?self.surface, "failed to project journaled tool execution; closing surface stream");
                    false
                }
            },
            Err(error) => {
                tracing::error!(%error, surface = ?self.surface, "failed to persist chat event; closing surface stream");
                false
            }
        }
    }

    fn on_journaled_event(&self, envelope: ChatEventEnvelope) -> bool {
        self.inner.on_journaled_event(envelope)
    }

    fn delivery_guarantee(&self) -> ChatDeliveryGuarantee {
        ChatDeliveryGuarantee::JournaledWithSemanticSafePoints
    }

    fn continuation_sink(&self) -> Option<Arc<dyn crate::chat_driver::ChatSink>> {
        self.inner.continuation_sink().map(|inner| {
            Self::wrap(
                inner,
                self.log.clone(),
                self.tool_execution_projector.clone(),
                self.surface,
                self.workspace_id.clone(),
                self.conversation_id.clone(),
                self.turn_id.clone(),
            )
        })
    }

    fn deferred_continuation_sink(&self) -> Option<Arc<dyn crate::chat_driver::ChatSink>> {
        Some(Self::wrap(
            Arc::new(JournalOnlySink),
            self.log.clone(),
            self.tool_execution_projector.clone(),
            self.surface,
            self.workspace_id.clone(),
            self.conversation_id.clone(),
            self.turn_id.clone(),
        ))
    }
}
