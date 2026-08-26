//! Application-owned ordered event journal for ordinary chat turns.
//!
//! The framework owns physical sequencing, segmentation, integrity, recovery,
//! durability and pruning. EKO owns stream identity, product retention pins and
//! projections for GUI, TUI, CLI, channels and boot recovery.

use crate::chat_driver::ChatDriverEvent;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedChatInput {
    pub input_id: String,
    pub workspace_id: String,
    pub conversation_id: String,
    pub text: String,
    pub attachments: Vec<crate::types::AttachmentData>,
    pub submitted_at_ms: u64,
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
    queued_inputs: HashMap<String, u64>,
    queued_latest: HashMap<String, u64>,
    queue_order: Vec<String>,
    awaiter_facts: HashMap<String, u64>,
    earliest: Option<u64>,
    #[cfg(test)]
    recovered_records: usize,
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

impl ChatEventLog {
    pub async fn append_async(
        self: &Arc<Self>,
        workspace_id: String,
        conversation_id: Option<String>,
        root_turn_id: String,
        event: ChatDriverEvent,
    ) -> Result<ChatEventEnvelope, ChatEventLogError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.append(
                &workspace_id,
                conversation_id.as_deref(),
                &root_turn_id,
                event,
            )
        })
        .await
        .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?
    }

    pub async fn settle_all_started_awaiter_deliveries_async(
        self: &Arc<Self>,
    ) -> Result<usize, ChatEventLogError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let recoveries = log.all_started_awaiter_deliveries()?;
            let mut settled = 0_usize;
            for (workspace_id, conversation_id, root_turn_id, acknowledgement) in recoveries {
                log.append(
                    &workspace_id,
                    conversation_id.as_deref(),
                    &root_turn_id,
                    ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement },
                )?;
                settled = settled.saturating_add(1);
            }
            Ok(settled)
        })
        .await
        .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?
    }

    pub async fn pending_awaiter_results_for_conversation_async(
        self: &Arc<Self>,
        workspace_id: String,
        conversation_id: String,
    ) -> Result<Vec<crate::tasks::task_runtime::command_cells::AwaiterResult>, ChatEventLogError>
    {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.pending_awaiter_results_for_conversation(&workspace_id, &conversation_id)
        })
        .await
        .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?
    }

    pub fn default_root() -> PathBuf {
        crate::data_root::user_data_path("chat-events")
    }

    pub fn at_default_root() -> Self {
        Self {
            root: Self::default_root(),
            retention: ChatEventRetention::default(),
            streams: DashMap::new(),
            stream_access: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            deletion_pause: None,
            #[cfg(test)]
            orphan_recovery_pause: None,
        }
    }

    pub fn open(
        root: impl Into<PathBuf>,
        retention: ChatEventRetention,
    ) -> Result<Self, ChatEventLogError> {
        if retention.segment_rollover_bytes == 0
            || retention.max_segments == 0
            || retention.max_replay_events == 0
        {
            return Err(ChatEventLogError::InvalidIdentity(
                "retention limits must be positive".to_string(),
            ));
        }
        let root = root.into();
        ensure_real_directory(&root, true)?;
        Ok(Self {
            root,
            retention,
            streams: DashMap::new(),
            stream_access: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            deletion_pause: None,
            #[cfg(test)]
            orphan_recovery_pause: None,
        })
    }

    pub fn append(
        &self,
        workspace_id: &str,
        conversation_id: Option<&str>,
        root_turn_id: &str,
        event: ChatDriverEvent,
    ) -> Result<ChatEventEnvelope, ChatEventLogError> {
        validate_event_stream_identity(workspace_id, conversation_id, &event)?;
        validate_driver_event(&event)?;
        if matches!(
            &event,
            ChatDriverEvent::InputQueued { input_id, .. }
                | ChatDriverEvent::InputRemoved { input_id }
                if input_id != root_turn_id
        ) {
            return Err(ChatEventLogError::InvalidIdentity(
                "queued chat input identity does not match the journal root".to_string(),
            ));
        }
        let selected_stream_id = stream_id(workspace_id, conversation_id, root_turn_id)?;
        let (turn_id, message_id) = event_identity(&event, root_turn_id);
        if message_id != root_turn_id {
            return Err(ChatEventLogError::InvalidIdentity(
                "event root message does not match the selected journal turn".to_string(),
            ));
        }
        let path = self.stream_dir(&selected_stream_id);
        let cached = self
            .stream_journal(&selected_stream_id, true)?
            .ok_or_else(|| corrupt(&path, "chat event stream authority was not created"))?;
        let mut guard = lock_cached_stream(&cached);
        let authority = guard
            .as_mut()
            .ok_or_else(|| corrupt(&path, "chat event stream authority was removed"))?;
        if self.retry_pending_barrier(authority, &selected_stream_id) {
            self.maintain_retention(authority, &selected_stream_id);
        }

        if let Some(fact_key) = awaiter_fact_key(&event)
            && let Some(sequence) = authority.pins.awaiter_facts.get(&fact_key).copied()
        {
            let record = authority
                .journal
                .replay_after(sequence.saturating_sub(1), 1)
                .map_err(|error| journal_error(&path, error))?
                .into_iter()
                .next()
                .filter(|record| record.sequence == sequence)
                .ok_or_else(|| {
                    corrupt(
                        &path,
                        format!("cached durable fact {fact_key} is missing at {sequence}"),
                    )
                })?;
            let expected = echo_agent::utils::canonical_json::canonical_json_bytes(&event)
                .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
            let actual =
                echo_agent::utils::canonical_json::canonical_json_bytes(&record.event.payload)
                    .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
            return if expected == actual {
                envelope_from_record(record, &path, &selected_stream_id)
            } else {
                Err(ChatEventLogError::InvalidEvent(format!(
                    "conflicting durable fact for {fact_key}"
                )))
            };
        }

        let persisted = PersistedChatEvent {
            schema_version: CHAT_EVENT_SCHEMA_VERSION,
            stream_id: selected_stream_id.clone(),
            workspace_id: workspace_id.to_string(),
            conversation_id: conversation_id.map(ToString::to_string),
            root_turn_id: root_turn_id.to_string(),
            turn_id,
            message_id,
            timestamp: Utc::now(),
            payload: event,
        };
        let durability = append_durability(&persisted.payload);
        let receipt = authority
            .journal
            .append_with_durability(persisted, durability)
            .map_err(|error| journal_error(&path, error))?;
        if let JournalDurabilityStatus::Degraded { error } = &receipt.durability {
            tracing::warn!(stream_id = %selected_stream_id, sequence = receipt.record.sequence, %error, "chat event committed with degraded durability; append will not be retried");
        }
        authority
            .pins
            .apply(receipt.record.sequence, receipt.record.event.as_ref());
        let mut maintain_retention = should_maintain_retention(durability, &receipt.durability);
        if should_mark_barrier_pending(durability, &receipt.durability) {
            authority.barrier_pending = true;
            maintain_retention = self.retry_pending_barrier(authority, &selected_stream_id);
        }
        let envelope = envelope_from_record(receipt.record, &path, &selected_stream_id)?;
        if maintain_retention {
            self.maintain_retention(authority, &selected_stream_id);
        }
        drop(guard);
        drop(cached);
        self.evict_inactive_streams(None);
        Ok(envelope)
    }

    pub fn replay(
        &self,
        workspace_id: &str,
        conversation_id: Option<&str>,
        turn_id: &str,
        after_cursor: u64,
    ) -> Result<ChatEventReplay, ChatEventLogError> {
        let selected_stream_id = stream_id(workspace_id, conversation_id, turn_id)?;
        let Some(cached) = self.stream_journal(&selected_stream_id, false)? else {
            return Ok(empty_replay());
        };
        let mut guard = lock_cached_stream(&cached);
        let Some(authority) = guard.as_mut() else {
            return Ok(empty_replay());
        };
        if self.retry_pending_barrier(authority, &selected_stream_id) {
            self.maintain_retention(authority, &selected_stream_id);
        }
        let journal = &authority.journal;
        let latest_cursor = journal.last_sequence();
        let retained_floor = journal.retention_metadata().retained_floor;
        if latest_cursor == 0 || latest_cursor < retained_floor {
            return Ok(empty_replay());
        }
        let floor_cursor = retained_floor.saturating_sub(1);
        let requested_after = after_cursor.max(floor_cursor);
        let replay_limit = u64::try_from(self.retention.max_replay_events).unwrap_or(u64::MAX);
        let cap_after = latest_cursor.saturating_sub(replay_limit);
        let effective_after = requested_after.max(cap_after);
        let path = self.stream_dir(&selected_stream_id);
        let records = journal
            .replay_after(effective_after, self.retention.max_replay_events)
            .map_err(|error| journal_error(&path, error))?;
        let events = records
            .into_iter()
            .map(|record| envelope_from_record(record, &path, &selected_stream_id))
            .collect::<Result<Vec<_>, _>>()?;
        let replay = ChatEventReplay {
            retained_earliest_cursor: Some(retained_floor),
            returned_earliest_cursor: events.first().map(|event| event.sequence),
            latest_cursor,
            truncated: after_cursor < floor_cursor || requested_after < cap_after,
            events,
        };
        drop(guard);
        drop(cached);
        self.evict_inactive_streams(None);
        Ok(replay)
    }

    /// Close ordinary-Chat command cells whose process owner disappeared.
    ///
    /// TaskRun cells are recovered by `TaskRuntimeStore`; this scans only the
    /// product chat journal so a Chat turn without a formal run still receives
    /// one durable Interrupted terminal after an application restart.
    pub fn recover_orphan_command_cells(&self) -> Result<usize, ChatEventLogError> {
        struct Recovery {
            workspace_id: String,
            conversation_id: Option<String>,
            root_turn_id: String,
            cell: crate::tasks::task_runtime::types::BackgroundCellState,
        }

        let mut recoveries = Vec::new();
        for stream in self.enumerate_streams()? {
            let Some(cached) = self.stream_journal(&stream.stream_id, false)? else {
                continue;
            };
            let mut guard = lock_cached_stream(&cached);
            let Some(authority) = guard.as_mut() else {
                continue;
            };
            let active = authority
                .pins
                .active_cells
                .values()
                .copied()
                .collect::<Vec<_>>();
            for sequence in active {
                let record = authority
                    .journal
                    .replay_after(sequence.saturating_sub(1), 1)
                    .map_err(|error| journal_error(&stream.path, error))?
                    .into_iter()
                    .next()
                    .filter(|record| record.sequence == sequence)
                    .ok_or_else(|| {
                        corrupt(
                            &stream.path,
                            format!("active command cell is missing at {sequence}"),
                        )
                    })?;
                validate_persisted_record(&record, &stream.path, Some(&stream.stream_id))?;
                let ChatDriverEvent::CommandCellStarted { cell } = &record.event.payload else {
                    return Err(corrupt(
                        &stream.path,
                        format!("active command cell pin at {sequence} is not a Started fact"),
                    ));
                };
                let mut cell = cell.as_ref().clone();
                cell.phase = crate::tasks::task_runtime::types::BackgroundCellPhase::Failed;
                cell.terminal_cause = Some(
                    crate::tasks::task_runtime::types::BackgroundCellTerminalCause::Interrupted,
                );
                cell.terminal_message =
                    Some("command cell was interrupted by process restart".to_string());
                cell.exit_code = None;
                if cell.artifact_status
                    == crate::tasks::task_runtime::types::BackgroundCellArtifactStatus::Writing
                {
                    cell.artifact_status =
                        crate::tasks::task_runtime::types::BackgroundCellArtifactStatus::Failed;
                    cell.artifact_message = Some(
                        "artifact finalization was interrupted by process restart".to_string(),
                    );
                }
                cell.finished_at = Some(Utc::now());
                recoveries.push(Recovery {
                    workspace_id: stream.first.workspace_id.clone(),
                    conversation_id: stream.first.conversation_id.clone(),
                    root_turn_id: stream.first.root_turn_id.clone(),
                    cell,
                });
            }
        }

        let mut recovered = 0_usize;
        #[cfg(test)]
        if let Some(pause) = &self.orphan_recovery_pause {
            pause.0.wait();
            pause.1.wait();
        }
        for recovery in recoveries {
            let cell_id = recovery.cell.cell_id.clone();
            let appended = self.append(
                &recovery.workspace_id,
                recovery.conversation_id.as_deref(),
                &recovery.root_turn_id,
                ChatDriverEvent::CommandCellSettled {
                    cell: Box::new(recovery.cell),
                },
            );
            match appended {
                Ok(_) => recovered = recovered.saturating_add(1),
                Err(ChatEventLogError::InvalidEvent(_)) => {
                    let replay = self.replay(
                        &recovery.workspace_id,
                        recovery.conversation_id.as_deref(),
                        &recovery.root_turn_id,
                        0,
                    )?;
                    let terminal_exists = replay.events.iter().any(|event| {
                        matches!(
                            &event.payload,
                            ChatDriverEvent::CommandCellSettled { cell }
                                if cell.cell_id == cell_id && !cell.is_active()
                        )
                    });
                    if !terminal_exists {
                        return Err(ChatEventLogError::InvalidEvent(format!(
                            "orphan command cell {cell_id} conflicted without a terminal fact"
                        )));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(recovered)
    }

    pub fn pending_awaiter_results(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        root_turn_id: &str,
    ) -> Result<Vec<crate::tasks::task_runtime::command_cells::AwaiterResult>, ChatEventLogError>
    {
        let selected_stream_id = stream_id(workspace_id, Some(conversation_id), root_turn_id)?;
        let Some(cached) = self.stream_journal(&selected_stream_id, false)? else {
            return Ok(Vec::new());
        };
        let mut guard = lock_cached_stream(&cached);
        let Some(authority) = guard.as_mut() else {
            return Ok(Vec::new());
        };
        if self.retry_pending_barrier(authority, &selected_stream_id) {
            self.maintain_retention(authority, &selected_stream_id);
        }
        let path = self.stream_dir(&selected_stream_id);
        let pending = authority
            .pins
            .pending_awaiters
            .iter()
            .map(|(key, sequence)| (key.clone(), *sequence))
            .collect::<BTreeMap<_, _>>();
        let mut results = Vec::with_capacity(pending.len());
        for (key, sequence) in pending {
            let record = authority
                .journal
                .replay_after(sequence.saturating_sub(1), 1)
                .map_err(|error| journal_error(&path, error))?
                .into_iter()
                .next()
                .filter(|record| record.sequence == sequence)
                .ok_or_else(|| {
                    corrupt(
                        &path,
                        format!("pending Awaiter {key} is missing at {sequence}"),
                    )
                })?;
            let envelope = envelope_from_record(record, &path, &selected_stream_id)?;
            let ChatDriverEvent::AwaiterResultReady { result } = envelope.payload else {
                return Err(corrupt(
                    &path,
                    format!("pending Awaiter {key} does not point to a Ready fact"),
                ));
            };
            if awaiter_receipt_key(&result.receipt) != key {
                return Err(corrupt(
                    &path,
                    format!("pending Awaiter {key} points to a different receipt"),
                ));
            }
            results.push(*result);
        }
        drop(guard);
        drop(cached);
        self.evict_inactive_streams(None);
        Ok(results)
    }

    pub fn pending_awaiter_results_for_conversation(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<Vec<crate::tasks::task_runtime::command_cells::AwaiterResult>, ChatEventLogError>
    {
        let mut pending = Vec::new();
        for stream in self.enumerate_streams()? {
            if stream.first.workspace_id == workspace_id
                && stream.first.conversation_id.as_deref() == Some(conversation_id)
            {
                pending.extend(self.pending_awaiter_results(
                    workspace_id,
                    conversation_id,
                    &stream.first.root_turn_id,
                )?);
            }
        }
        pending.sort_by(|left, right| {
            left.receipt
                .started_at
                .cmp(&right.receipt.started_at)
                .then_with(|| left.receipt.execution_id.cmp(&right.receipt.execution_id))
        });
        Ok(pending)
    }

    fn all_started_awaiter_deliveries(
        &self,
    ) -> Result<Vec<StartedAwaiterDelivery>, ChatEventLogError> {
        let mut started = Vec::new();
        for stream in self.enumerate_streams()? {
            let Some(cached) = self.stream_journal(&stream.stream_id, false)? else {
                continue;
            };
            let guard = lock_cached_stream(&cached);
            let Some(authority) = guard.as_ref() else {
                continue;
            };
            started.extend(
                authority
                    .pins
                    .started_awaiters
                    .values()
                    .map(|(_, acknowledgement)| {
                        (
                            stream.first.workspace_id.clone(),
                            stream.first.conversation_id.clone(),
                            stream.first.root_turn_id.clone(),
                            acknowledgement.clone(),
                        )
                    }),
            );
        }
        Ok(started)
    }

    pub fn enqueue_chat_input(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        input_id: &str,
        text: String,
        attachments: Vec<crate::types::AttachmentData>,
    ) -> Result<QueuedChatInput, ChatEventLogError> {
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(ChatEventLogError::InvalidEvent(
                "queued chat input must contain text or attachments".to_string(),
            ));
        }
        let submitted_at_ms = echo_agent::utils::time::now_millis();
        self.append(
            workspace_id,
            Some(conversation_id),
            input_id,
            ChatDriverEvent::InputQueued {
                input_id: input_id.to_string(),
                text: text.clone(),
                attachments: attachments.clone(),
                submitted_at_ms,
            },
        )?;
        Ok(QueuedChatInput {
            input_id: input_id.to_string(),
            workspace_id: workspace_id.to_string(),
            conversation_id: conversation_id.to_string(),
            text,
            attachments,
            submitted_at_ms,
        })
    }

    pub fn queued_chat_inputs(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<Vec<QueuedChatInput>, ChatEventLogError> {
        let selected_stream_id = stream_id(workspace_id, Some(conversation_id), conversation_id)?;
        let Some(cached) = self.stream_journal(&selected_stream_id, false)? else {
            return Ok(Vec::new());
        };
        let mut guard = lock_cached_stream(&cached);
        let Some(authority) = guard.as_mut() else {
            return Ok(Vec::new());
        };
        if self.retry_pending_barrier(authority, &selected_stream_id) {
            self.maintain_retention(authority, &selected_stream_id);
        }
        let path = self.stream_dir(&selected_stream_id);
        let mut queued = Vec::with_capacity(authority.pins.queued_latest.len());
        for input_id in &authority.pins.queue_order {
            let Some(sequence) = authority.pins.queued_latest.get(input_id).copied() else {
                continue;
            };
            let record = authority
                .journal
                .replay_after(sequence.saturating_sub(1), 1)
                .map_err(|error| journal_error(&path, error))?
                .into_iter()
                .next()
                .filter(|record| record.sequence == sequence)
                .ok_or_else(|| {
                    corrupt(
                        &path,
                        format!("queued input {input_id} is missing at {sequence}"),
                    )
                })?;
            let envelope = envelope_from_record(record, &path, &selected_stream_id)?;
            let ChatDriverEvent::InputQueued {
                input_id: stored_input_id,
                text,
                attachments,
                submitted_at_ms,
            } = envelope.payload
            else {
                return Err(corrupt(
                    &path,
                    format!("queued input {input_id} does not point to an InputQueued fact"),
                ));
            };
            if stored_input_id != *input_id {
                return Err(corrupt(
                    &path,
                    format!("queued input {input_id} points to {stored_input_id}"),
                ));
            }
            queued.push(QueuedChatInput {
                input_id: stored_input_id,
                workspace_id: workspace_id.to_string(),
                conversation_id: conversation_id.to_string(),
                text,
                attachments,
                submitted_at_ms,
            });
        }
        drop(guard);
        drop(cached);
        self.evict_inactive_streams(None);
        Ok(queued)
    }

    pub fn remove_queued_chat_input(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        input_id: &str,
    ) -> Result<(), ChatEventLogError> {
        self.append(
            workspace_id,
            Some(conversation_id),
            input_id,
            ChatDriverEvent::InputRemoved {
                input_id: input_id.to_string(),
            },
        )?;
        Ok(())
    }

    pub fn reorder_queued_chat_inputs(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        input_ids: Vec<String>,
    ) -> Result<(), ChatEventLogError> {
        if input_ids.is_empty()
            || input_ids.iter().any(|input_id| input_id.trim().is_empty())
            || has_duplicate_ids(&input_ids)
        {
            return Err(ChatEventLogError::InvalidEvent(
                "queued input order must contain unique non-empty identities".to_string(),
            ));
        }
        let root_turn_id = format!("queue-order:{}", uuid::Uuid::new_v4());
        self.append(
            workspace_id,
            Some(conversation_id),
            &root_turn_id,
            ChatDriverEvent::InputReordered { input_ids },
        )?;
        Ok(())
    }

    pub fn remove_conversation(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<(), ChatEventLogError> {
        if workspace_id.trim().is_empty() || conversation_id.trim().is_empty() {
            return Err(ChatEventLogError::InvalidIdentity(
                "workspace_id and conversation_id must not be empty".to_string(),
            ));
        }
        if !ensure_real_directory(&self.root, false)? {
            return Ok(());
        }
        let selected_stream_id = stream_id(workspace_id, Some(conversation_id), conversation_id)?;
        let path = self.stream_dir(&selected_stream_id);
        if ensure_real_directory(&path, false)? {
            let _validated = self.stream_journal(&selected_stream_id, false)?;
            self.remove_stream(&selected_stream_id, &path)?;
        }
        Ok(())
    }

    pub fn remove_workspace(&self, workspace_id: &str) -> Result<(), ChatEventLogError> {
        if workspace_id.trim().is_empty() {
            return Err(ChatEventLogError::InvalidIdentity(
                "workspace_id must not be empty".to_string(),
            ));
        }
        if !ensure_real_directory(&self.root, false)? {
            return Ok(());
        }
        for stream in self.enumerate_streams()? {
            if stream.first.workspace_id == workspace_id {
                self.remove_stream(&stream.stream_id, &stream.path)?;
            }
        }
        Ok(())
    }

    fn maintain_retention(&self, authority: &mut StreamAuthority, stream_id: &str) {
        let metadata = authority.journal.retention_metadata();
        let segments = authority.journal.segments();
        if segments.len() <= self.retention.max_segments && !metadata.cleanup_pending {
            return;
        }
        let natural_keep = segments
            .get(segments.len().saturating_sub(self.retention.max_segments))
            .map(|segment| segment.start_sequence)
            .unwrap_or(metadata.retained_floor);
        let keep_from = authority
            .pins
            .earliest()
            .map_or(natural_keep, |pin| natural_keep.min(pin));
        match authority.journal.prune_closed_segments_before(keep_from) {
            Ok(receipt) => {
                authority.pins.discard_before(receipt.retained_floor);
                if let JournalPruneCommitStatus::Degraded { error } = receipt.commit {
                    tracing::warn!(%error, %stream_id, retained_floor = receipt.retained_floor, "chat event retention marker committed with a degraded barrier");
                }
                if let JournalPhysicalCleanupStatus::Degraded { error } = receipt.cleanup {
                    tracing::warn!(%error, %stream_id, retained_floor = receipt.retained_floor, "chat event retention cleanup remains pending");
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, %stream_id, "chat event retention remains pending after a committed safe point")
            }
        }
    }

    fn retry_pending_barrier(&self, authority: &mut StreamAuthority, stream_id: &str) -> bool {
        if !authority.barrier_pending {
            return false;
        }
        match authority.journal.sync_data() {
            Ok(()) => {
                authority.barrier_pending = false;
                true
            }
            Err(error) => {
                tracing::warn!(error = %error, %stream_id, "chat event durability barrier remains pending; committed event will not be retried");
                false
            }
        }
    }

    fn touch_stream(&self, stream_id: &str) {
        let mut access = self
            .stream_access
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        access.retain(|cached| cached != stream_id);
        access.push_back(stream_id.to_string());
    }

    fn evict_inactive_streams(&self, protected_stream: Option<&str>) {
        let mut access = self
            .stream_access
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut attempts = access.len();
        while access.len() > MAX_CACHED_STREAMS && attempts > 0 {
            attempts = attempts.saturating_sub(1);
            let Some(candidate) = access.pop_front() else {
                break;
            };
            if protected_stream == Some(candidate.as_str()) {
                access.push_back(candidate);
                continue;
            }
            let Some(cached) = self.streams.get(&candidate) else {
                continue;
            };
            let can_evict = cached.value().try_lock().is_ok_and(|authority| {
                authority
                    .as_ref()
                    .is_none_or(|authority| !authority.barrier_pending)
            });
            drop(cached);
            if can_evict {
                self.streams.remove(&candidate);
            } else {
                access.push_back(candidate);
            }
        }
    }

    fn forget_stream(&self, stream_id: &str) {
        self.stream_access
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|cached| cached != stream_id);
    }

    #[cfg(test)]
    fn with_deletion_pause(mut self, pause: Arc<(std::sync::Barrier, std::sync::Barrier)>) -> Self {
        self.deletion_pause = Some(pause);
        self
    }

    #[cfg(test)]
    fn with_orphan_recovery_pause(
        mut self,
        pause: Arc<(std::sync::Barrier, std::sync::Barrier)>,
    ) -> Self {
        self.orphan_recovery_pause = Some(pause);
        self
    }

    fn stream_journal(
        &self,
        stream_id: &str,
        create: bool,
    ) -> Result<Option<CachedStreamJournal>, ChatEventLogError> {
        if !ensure_real_directory(&self.root, create)? {
            return Ok(None);
        }
        let stream_dir = self.stream_dir(stream_id);
        if !ensure_real_directory(&stream_dir, create)? {
            return Ok(None);
        }
        if let Some(existing) = self.streams.get(stream_id) {
            let cached = Arc::clone(existing.value());
            drop(existing);
            let mut guard = lock_cached_stream(&cached);
            match guard.as_ref() {
                Some(authority) => {
                    validate_authority_config(authority, stream_id, self.retention, &stream_dir)?;
                    validate_authority_storage(authority, &stream_dir)?;
                }
                None => {
                    *guard = Some(open_stream_authority(
                        &stream_dir,
                        stream_id,
                        self.retention,
                    )?);
                }
            }
            drop(guard);
            self.touch_stream(stream_id);
            self.evict_inactive_streams(Some(stream_id));
            return Ok(Some(cached));
        }
        let canonical = fs::canonicalize(&stream_dir).map_err(|source| ChatEventLogError::Io {
            path: stream_dir.clone(),
            source,
        })?;
        let shared = {
            let mut registry = stream_authority_registry().lock().map_err(|error| {
                corrupt(&stream_dir, format!("stream registry poisoned: {error}"))
            })?;
            if registry.len() > MAX_REGISTRY_ENTRIES_BEFORE_PRUNE {
                registry.retain(|_, authority| authority.strong_count() > 0);
            }
            if let Some(shared) = registry.get(&canonical).and_then(Weak::upgrade) {
                shared
            } else {
                let shared = Arc::new(Mutex::new(None));
                registry.insert(canonical, Arc::downgrade(&shared));
                shared
            }
        };
        {
            let mut guard = lock_cached_stream(&shared);
            match guard.as_ref() {
                Some(authority) => {
                    validate_authority_config(authority, stream_id, self.retention, &stream_dir)?;
                    validate_authority_storage(authority, &stream_dir)?;
                }
                None => {
                    *guard = Some(open_stream_authority(
                        &stream_dir,
                        stream_id,
                        self.retention,
                    )?);
                }
            }
        }
        let entry = self
            .streams
            .entry(stream_id.to_string())
            .or_insert_with(|| Arc::clone(&shared));
        let cached = Arc::clone(entry.value());
        drop(entry);
        self.touch_stream(stream_id);
        self.evict_inactive_streams(Some(stream_id));
        Ok(Some(cached))
    }

    fn enumerate_streams(&self) -> Result<Vec<EnumeratedStream>, ChatEventLogError> {
        if !ensure_real_directory(&self.root, false)? {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&self.root).map_err(|source| ChatEventLogError::Io {
            path: self.root.clone(),
            source,
        })?;
        let mut streams = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| ChatEventLogError::Io {
                path: self.root.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| ChatEventLogError::Io {
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink() {
                return Err(corrupt(&path, "chat event stream must not be a symlink"));
            }
            if !metadata.is_dir() {
                continue;
            }
            let journal = StreamJournal::open(
                &path,
                self.retention.segment_rollover_bytes,
                FileDurability::Flush,
            )
            .map_err(|error| journal_error(&path, error))?;
            let floor = journal.retention_metadata().retained_floor;
            let first = journal
                .replay_after(floor.saturating_sub(1), 1)
                .map_err(|error| journal_error(&path, error))?
                .into_iter()
                .next()
                .map(|record| envelope_from_record_for_enumeration(record, &path))
                .transpose()?;
            if let Some(first) = first {
                streams.push(EnumeratedStream {
                    stream_id: first.stream_id.clone(),
                    path,
                    first,
                });
            }
        }
        Ok(streams)
    }

    fn remove_stream(&self, stream_id: &str, path: &Path) -> Result<(), ChatEventLogError> {
        self.forget_stream(stream_id);
        let canonical = fs::canonicalize(path).map_err(|source| ChatEventLogError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let local = self.streams.remove(stream_id).map(|(_, cached)| cached);
        let registered = stream_authority_registry()
            .lock()
            .map_err(|error| corrupt(path, format!("stream registry poisoned: {error}")))?
            .get(&canonical)
            .and_then(Weak::upgrade);
        if let Some(cached) = local.or(registered) {
            let mut guard = lock_cached_stream(&cached);
            drop(guard.take());
            #[cfg(test)]
            if let Some(pause) = &self.deletion_pause {
                pause.0.wait();
                pause.1.wait();
            }
            let result = match fs::remove_dir_all(path) {
                Ok(()) => Ok(()),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(ChatEventLogError::Io {
                    path: path.to_path_buf(),
                    source,
                }),
            };
            drop(guard);
            if Arc::strong_count(&cached) == 1 {
                stream_authority_registry()
                    .lock()
                    .map_err(|error| corrupt(path, format!("stream registry poisoned: {error}")))?
                    .remove(&canonical);
            }
            return result;
        }
        fs::remove_dir_all(path).map_err(|source| ChatEventLogError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn stream_dir(&self, stream_id: &str) -> PathBuf {
        self.root.join(digest(stream_id.as_bytes()))
    }
}

struct EnumeratedStream {
    stream_id: String,
    path: PathBuf,
    first: ChatEventEnvelope,
}

fn open_stream_authority(
    path: &Path,
    expected_stream_id: &str,
    retention: ChatEventRetention,
) -> Result<StreamAuthority, ChatEventLogError> {
    let journal = StreamJournal::open(
        path,
        retention.segment_rollover_bytes,
        FileDurability::Flush,
    )
    .map_err(|error| journal_error(path, error))?;
    let pins = RetentionPins::recover(&journal, path, expected_stream_id)?;
    Ok(StreamAuthority {
        expected_stream_id: expected_stream_id.to_string(),
        retention,
        journal,
        pins,
        barrier_pending: false,
    })
}

fn validate_authority_config(
    authority: &StreamAuthority,
    expected_stream_id: &str,
    retention: ChatEventRetention,
    path: &Path,
) -> Result<(), ChatEventLogError> {
    if authority.expected_stream_id != expected_stream_id || authority.retention != retention {
        return Err(corrupt(
            path,
            "chat event stream is already open with a different identity or retention configuration",
        ));
    }
    Ok(())
}

fn validate_authority_storage(
    authority: &StreamAuthority,
    path: &Path,
) -> Result<(), ChatEventLogError> {
    let floor = authority.journal.retention_metadata().retained_floor;
    if let Some(record) = authority
        .journal
        .replay_after(floor.saturating_sub(1), 1)
        .map_err(|error| journal_error(path, error))?
        .first()
    {
        validate_persisted_record(record, path, Some(&authority.expected_stream_id))?;
    }
    Ok(())
}

impl RetentionPins {
    fn recover(
        journal: &StreamJournal,
        path: &Path,
        expected_stream_id: &str,
    ) -> Result<Self, ChatEventLogError> {
        let mut projection = Self::default();
        let mut cursor = journal
            .retention_metadata()
            .retained_floor
            .saturating_sub(1);
        loop {
            let batch = journal
                .replay_after(cursor, REPLAY_BATCH_SIZE)
                .map_err(|error| journal_error(path, error))?;
            if batch.is_empty() {
                return Ok(projection);
            }
            let next_cursor = batch.last().map(|record| record.sequence).unwrap_or(cursor);
            if next_cursor <= cursor {
                return Err(corrupt(
                    path,
                    "framework journal pin recovery did not advance its cursor",
                ));
            }
            for record in &batch {
                validate_persisted_record(record, path, Some(expected_stream_id))?;
                projection.apply(record.sequence, record.event.as_ref());
                #[cfg(test)]
                {
                    projection.recovered_records = projection.recovered_records.saturating_add(1);
                }
            }
            cursor = next_cursor;
            if batch.len() < REPLAY_BATCH_SIZE {
                return Ok(projection);
            }
        }
    }

    fn apply(&mut self, sequence: u64, event: &PersistedChatEvent) {
        self.cursor = sequence;
        if let Some(fact_key) = awaiter_fact_key(&event.payload) {
            self.awaiter_facts.entry(fact_key).or_insert(sequence);
        }
        match &event.payload {
            ChatDriverEvent::CommandCellStarted { cell } => {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    self.active_cells.entry(cell.cell_id.clone())
                {
                    entry.insert(sequence);
                    self.earliest = Some(self.earliest.map_or(sequence, |old| old.min(sequence)));
                }
            }
            ChatDriverEvent::CommandCellSettled { cell } => {
                let removed = self.active_cells.remove(&cell.cell_id);
                self.refresh_earliest_if_removed(removed);
            }
            ChatDriverEvent::AwaiterResultReady { result } => {
                let key = awaiter_receipt_key(&result.receipt);
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    self.pending_awaiters.entry(key)
                {
                    entry.insert(sequence);
                    self.earliest = Some(self.earliest.map_or(sequence, |old| old.min(sequence)));
                }
            }
            ChatDriverEvent::AwaiterResultDeliveryStarted { acknowledgement } => {
                let key = awaiter_ack_key(acknowledgement);
                let removed = self.pending_awaiters.remove(&key);
                self.started_awaiters
                    .entry(key)
                    .or_insert_with(|| (sequence, acknowledgement.clone()));
                self.refresh_earliest_if_removed(removed);
                self.earliest = Some(self.earliest.map_or(sequence, |old| old.min(sequence)));
            }
            ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement } => {
                let key = awaiter_ack_key(acknowledgement);
                let pending = self.pending_awaiters.remove(&key);
                let started = self
                    .started_awaiters
                    .remove(&key)
                    .map(|(sequence, _)| sequence);
                self.refresh_earliest_if_removed(pending.or(started));
            }
            ChatDriverEvent::InputQueued { input_id, .. } => {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    self.queued_inputs.entry(input_id.clone())
                {
                    entry.insert(sequence);
                    if !self.queue_order.contains(input_id) {
                        self.queue_order.push(input_id.clone());
                    }
                    self.earliest = Some(self.earliest.map_or(sequence, |old| old.min(sequence)));
                }
                self.queued_latest.insert(input_id.clone(), sequence);
            }
            ChatDriverEvent::InputRemoved { input_id } => {
                let removed = self.queued_inputs.remove(input_id);
                self.queued_latest.remove(input_id);
                self.queue_order.retain(|queued| queued != input_id);
                self.refresh_earliest_if_removed(removed);
            }
            ChatDriverEvent::InputReordered { input_ids } => {
                let mut next = Vec::new();
                for input_id in input_ids {
                    if self.queued_inputs.contains_key(input_id) && !next.contains(input_id) {
                        next.push(input_id.clone());
                    }
                }
                for input_id in &self.queue_order {
                    if self.queued_inputs.contains_key(input_id) && !next.contains(input_id) {
                        next.push(input_id.clone());
                    }
                }
                self.queue_order = next;
            }
            _ => {}
        }
    }

    fn earliest(&self) -> Option<u64> {
        self.earliest
    }

    fn discard_before(&mut self, retained_floor: u64) {
        self.awaiter_facts
            .retain(|_, sequence| *sequence >= retained_floor);
        self.pending_awaiters
            .retain(|_, sequence| *sequence >= retained_floor);
        self.started_awaiters
            .retain(|_, (sequence, _)| *sequence >= retained_floor);
        self.active_cells
            .retain(|_, sequence| *sequence >= retained_floor);
        self.queued_inputs
            .retain(|_, sequence| *sequence >= retained_floor);
        self.queued_latest
            .retain(|_, sequence| *sequence >= retained_floor);
        self.queue_order
            .retain(|input_id| self.queued_inputs.contains_key(input_id));
        self.refresh_earliest();
    }

    fn refresh_earliest_if_removed(&mut self, removed: Option<u64>) {
        if removed.is_some_and(|sequence| self.earliest == Some(sequence)) {
            self.refresh_earliest();
        }
    }

    fn refresh_earliest(&mut self) {
        self.earliest = self
            .pending_awaiters
            .values()
            .chain(self.started_awaiters.values().map(|(sequence, _)| sequence))
            .chain(self.active_cells.values())
            .chain(self.queued_inputs.values())
            .copied()
            .min();
    }
}

fn envelope_from_record(
    record: JournalRecord<PersistedChatEvent>,
    path: &Path,
    expected_stream_id: &str,
) -> Result<ChatEventEnvelope, ChatEventLogError> {
    validate_persisted_record(&record, path, Some(expected_stream_id))?;
    envelope_from_validated_record(record, path)
}

fn envelope_from_record_for_enumeration(
    record: JournalRecord<PersistedChatEvent>,
    path: &Path,
) -> Result<ChatEventEnvelope, ChatEventLogError> {
    validate_persisted_record(&record, path, None)?;
    let expected_directory = digest(record.event.stream_id.as_bytes());
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_directory.as_str()) {
        return Err(corrupt(
            path,
            "chat event directory does not match its persisted stream identity",
        ));
    }
    envelope_from_validated_record(record, path)
}

fn validate_persisted_record(
    record: &JournalRecord<PersistedChatEvent>,
    path: &Path,
    expected_stream_id: Option<&str>,
) -> Result<(), ChatEventLogError> {
    let persisted = record.event.as_ref();
    if persisted.schema_version != CHAT_EVENT_SCHEMA_VERSION {
        return Err(corrupt(
            path,
            format!("unsupported schema version {}", persisted.schema_version),
        ));
    }
    validate_driver_event(&persisted.payload).map_err(|error| corrupt(path, error.to_string()))?;
    validate_event_stream_identity(
        &persisted.workspace_id,
        persisted.conversation_id.as_deref(),
        &persisted.payload,
    )
    .map_err(|error| corrupt(path, error.to_string()))?;
    let derived_stream = stream_id(
        &persisted.workspace_id,
        persisted.conversation_id.as_deref(),
        &persisted.root_turn_id,
    )
    .map_err(|error| corrupt(path, error.to_string()))?;
    let (expected_turn_id, expected_message_id) =
        event_identity(&persisted.payload, &persisted.root_turn_id);
    if persisted.stream_id != derived_stream
        || expected_stream_id.is_some_and(|expected| persisted.stream_id != expected)
        || persisted.turn_id != expected_turn_id
        || persisted.message_id != expected_message_id
        || persisted.message_id != persisted.root_turn_id
    {
        return Err(corrupt(
            path,
            "persisted chat identity does not match its payload, directory, or stream",
        ));
    }
    Ok(())
}

fn envelope_from_validated_record(
    record: JournalRecord<PersistedChatEvent>,
    path: &Path,
) -> Result<ChatEventEnvelope, ChatEventLogError> {
    let sequence = record.sequence;
    let persisted = Arc::try_unwrap(record.event).map_err(|_| {
        corrupt(
            path,
            "framework journal record payload was unexpectedly shared during projection",
        )
    })?;
    let content_hash = envelope_content_hash(EnvelopeIntegrity {
        schema_version: CHAT_EVENT_SCHEMA_VERSION,
        sequence,
        stream_id: &persisted.stream_id,
        workspace_id: &persisted.workspace_id,
        conversation_id: persisted.conversation_id.as_deref(),
        root_turn_id: &persisted.root_turn_id,
        turn_id: &persisted.turn_id,
        message_id: &persisted.message_id,
        timestamp: persisted.timestamp,
        payload: &persisted.payload,
    })?;
    Ok(ChatEventEnvelope {
        schema_version: CHAT_EVENT_SCHEMA_VERSION,
        event_id: stable_event_id(&persisted.stream_id, sequence, &content_hash),
        content_hash,
        sequence,
        stream_id: persisted.stream_id,
        workspace_id: persisted.workspace_id,
        conversation_id: persisted.conversation_id,
        root_turn_id: persisted.root_turn_id,
        turn_id: persisted.turn_id,
        message_id: persisted.message_id,
        timestamp: persisted.timestamp,
        payload: persisted.payload,
    })
}

fn empty_replay() -> ChatEventReplay {
    ChatEventReplay {
        events: Vec::new(),
        retained_earliest_cursor: None,
        returned_earliest_cursor: None,
        latest_cursor: 0,
        truncated: false,
    }
}

fn corrupt(path: &Path, message: impl Into<String>) -> ChatEventLogError {
    ChatEventLogError::Corrupt {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn journal_error(path: &Path, error: impl std::fmt::Display) -> ChatEventLogError {
    corrupt(path, error.to_string())
}

fn lock_cached_stream(stream: &CachedStreamJournal) -> MutexGuard<'_, Option<StreamAuthority>> {
    stream.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("chat event stream lock was poisoned; recovering authority");
        poisoned.into_inner()
    })
}

fn should_maintain_retention(requested: FileDurability, status: &JournalDurabilityStatus) -> bool {
    matches!(requested, FileDurability::SyncData)
        && matches!(status, JournalDurabilityStatus::Confirmed)
}

fn should_mark_barrier_pending(
    requested: FileDurability,
    status: &JournalDurabilityStatus,
) -> bool {
    matches!(requested, FileDurability::SyncData)
        && matches!(status, JournalDurabilityStatus::Degraded { .. })
}

fn append_durability(event: &ChatDriverEvent) -> FileDurability {
    match event {
        ChatDriverEvent::Agent(envelope) => match &envelope.payload {
            echo_agent::agent::AgentEvent::ToolCall { .. }
            | echo_agent::agent::AgentEvent::ToolResult { .. }
            | echo_agent::agent::AgentEvent::FinalAnswer(_)
            | echo_agent::agent::AgentEvent::Cancelled
            | echo_agent::agent::AgentEvent::Error { .. }
            | echo_agent::agent::AgentEvent::ContextCompressed { .. } => FileDurability::SyncData,
            _ => FileDurability::Flush,
        },
        ChatDriverEvent::TurnStatus { status }
            if matches!(status.as_str(), "completed" | "failed" | "cancelled") =>
        {
            FileDurability::SyncData
        }
        ChatDriverEvent::Execution(event)
            if matches!(
                event.event,
                crate::tasks::task_runtime::types::RuntimeEventKind::Running
                    | crate::tasks::task_runtime::types::RuntimeEventKind::ThinkingStarted
                    | crate::tasks::task_runtime::types::RuntimeEventKind::ThinkingDelta
                    | crate::tasks::task_runtime::types::RuntimeEventKind::ThinkingEnded
                    | crate::tasks::task_runtime::types::RuntimeEventKind::TokenDelta
                    | crate::tasks::task_runtime::types::RuntimeEventKind::Usage
                    | crate::tasks::task_runtime::types::RuntimeEventKind::ToolOutput
                    | crate::tasks::task_runtime::types::RuntimeEventKind::Note
            ) =>
        {
            FileDurability::Flush
        }
        ChatDriverEvent::Execution(_)
        | ChatDriverEvent::ExecutionPath { .. }
        | ChatDriverEvent::TurnConfiguration { .. }
        | ChatDriverEvent::ExtensionReceipt(_)
        | ChatDriverEvent::Interrupt { .. }
        | ChatDriverEvent::CommandCellStarted { .. }
        | ChatDriverEvent::CommandCellSettled { .. }
        | ChatDriverEvent::AwaiterResultReady { .. }
        | ChatDriverEvent::AwaiterResultDeliveryStarted { .. }
        | ChatDriverEvent::AwaiterResultAcknowledged { .. }
        | ChatDriverEvent::InputQueued { .. }
        | ChatDriverEvent::InputRemoved { .. }
        | ChatDriverEvent::InputReordered { .. }
        | ChatDriverEvent::ApprovalRequest { .. }
        | ChatDriverEvent::InputRequest { .. }
        | ChatDriverEvent::SelectionRequest { .. }
        | ChatDriverEvent::ContextCompressed { .. } => FileDurability::SyncData,
        ChatDriverEvent::TurnStatus { .. } => FileDurability::Flush,
    }
}

fn ensure_real_directory(path: &Path, create: bool) -> Result<bool, ChatEventLogError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound && !create => {
            return Ok(false);
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|source| ChatEventLogError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            fs::symlink_metadata(path).map_err(|source| ChatEventLogError::Io {
                path: path.to_path_buf(),
                source,
            })?
        }
        Err(source) => {
            return Err(ChatEventLogError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(corrupt(
            path,
            "chat event directory path is not a real directory",
        ));
    }
    Ok(true)
}

fn stream_id(
    workspace_id: &str,
    conversation_id: Option<&str>,
    root_turn_id: &str,
) -> Result<String, ChatEventLogError> {
    if workspace_id.trim().is_empty() || root_turn_id.trim().is_empty() {
        return Err(ChatEventLogError::InvalidIdentity(
            "workspace_id and root_turn_id must not be empty".to_string(),
        ));
    }
    if conversation_id.is_some_and(|value| value.trim().is_empty()) {
        return Err(ChatEventLogError::InvalidIdentity(
            "conversation_id must not be empty".to_string(),
        ));
    }
    match conversation_id {
        Some(conversation_id) => serde_json::to_string(&(workspace_id, conversation_id)),
        None => serde_json::to_string(&(workspace_id, root_turn_id)),
    }
    .map_err(|error| ChatEventLogError::Serialization(error.to_string()))
}

fn event_identity(event: &ChatDriverEvent, root_turn_id: &str) -> (String, String) {
    match event {
        ChatDriverEvent::Agent(envelope) => (
            envelope.turn_id.as_str().to_string(),
            envelope
                .message_id
                .as_ref()
                .map(|message_id| message_id.as_str().to_string())
                .unwrap_or_else(|| root_turn_id.to_string()),
        ),
        _ => (root_turn_id.to_string(), root_turn_id.to_string()),
    }
}

fn awaiter_receipt_key(
    receipt: &crate::tasks::task_runtime::command_cells::AwaiterWatchReceipt,
) -> String {
    format!(
        "{}:{}:{}",
        receipt.execution_id, receipt.attempt, receipt.watch_generation
    )
}

fn awaiter_ack_key(
    acknowledgement: &crate::tasks::task_runtime::command_cells::AwaiterResultAcknowledgement,
) -> String {
    format!(
        "{}:{}:{}",
        acknowledgement.execution_id, acknowledgement.attempt, acknowledgement.watch_generation
    )
}

fn awaiter_fact_key(event: &ChatDriverEvent) -> Option<String> {
    match event {
        ChatDriverEvent::CommandCellStarted { cell } => {
            Some(format!("cell_started:{}", cell.cell_id))
        }
        ChatDriverEvent::CommandCellSettled { cell } => {
            Some(format!("cell_settled:{}", cell.cell_id))
        }
        ChatDriverEvent::AwaiterResultReady { result } => {
            Some(format!("ready:{}", awaiter_receipt_key(&result.receipt)))
        }
        ChatDriverEvent::AwaiterResultDeliveryStarted { acknowledgement } => {
            Some(format!("started:{}", awaiter_ack_key(acknowledgement)))
        }
        ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement } => {
            Some(format!("ack:{}", awaiter_ack_key(acknowledgement)))
        }
        _ => None,
    }
}

fn validate_event_stream_identity(
    workspace_id: &str,
    conversation_id: Option<&str>,
    event: &ChatDriverEvent,
) -> Result<(), ChatEventLogError> {
    match event {
        ChatDriverEvent::Agent(envelope) => {
            let event_conversation_id = envelope
                .conversation_id
                .as_ref()
                .map(|identity| identity.as_str());
            if event_conversation_id != conversation_id {
                return Err(ChatEventLogError::InvalidIdentity(format!(
                    "framework envelope conversation {event_conversation_id:?} does not match journal stream {conversation_id:?}"
                )));
            }
        }
        ChatDriverEvent::Execution(execution)
            if execution.workspace_id != workspace_id
                || Some(execution.conversation_id.as_str()) != conversation_id =>
        {
            return Err(ChatEventLogError::InvalidIdentity(
                "execution event address does not match journal stream".to_string(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn validate_driver_event(event: &ChatDriverEvent) -> Result<(), ChatEventLogError> {
    if let ChatDriverEvent::Agent(envelope) = event {
        if envelope.schema_version != echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION {
            return Err(ChatEventLogError::InvalidEvent(format!(
                "unsupported framework event schema version {}",
                envelope.schema_version
            )));
        }
        if envelope.sequence == 0
            || envelope.event_id.as_str().trim().is_empty()
            || envelope.content_hash.trim().is_empty()
            || envelope.stream_id.as_str().trim().is_empty()
            || envelope.turn_id.as_str().trim().is_empty()
        {
            return Err(ChatEventLogError::InvalidEvent(
                "framework event identity, hash, and sequence must be populated".to_string(),
            ));
        }
    }
    if let ChatDriverEvent::TurnStatus { status } = event
        && !matches!(
            status.as_str(),
            "idle"
                | "running"
                | "thinking"
                | "using_tool"
                | "waiting_approval"
                | "waiting_input"
                | "completed"
                | "failed"
                | "cancelled"
        )
    {
        return Err(ChatEventLogError::InvalidEvent(format!(
            "unknown turn status {status:?} for chat event schema {CHAT_EVENT_SCHEMA_VERSION}"
        )));
    }
    if let ChatDriverEvent::InputQueued {
        input_id,
        text,
        attachments,
        ..
    } = event
        && (input_id.trim().is_empty() || (text.trim().is_empty() && attachments.is_empty()))
    {
        return Err(ChatEventLogError::InvalidEvent(
            "queued chat input has an invalid identity or empty payload".to_string(),
        ));
    }
    if let ChatDriverEvent::InputRemoved { input_id } = event
        && input_id.trim().is_empty()
    {
        return Err(ChatEventLogError::InvalidEvent(
            "removed chat input id must not be empty".to_string(),
        ));
    }
    if let ChatDriverEvent::InputReordered { input_ids } = event
        && (input_ids.is_empty()
            || input_ids.iter().any(|input_id| input_id.trim().is_empty())
            || has_duplicate_ids(input_ids))
    {
        return Err(ChatEventLogError::InvalidEvent(
            "queued input order contains an empty or duplicate identity".to_string(),
        ));
    }
    match event {
        ChatDriverEvent::CommandCellStarted { cell }
            if cell.cell_id.trim().is_empty() || !cell.is_active() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "command-cell Started fact must have an active typed state".to_string(),
            ))
        }
        ChatDriverEvent::CommandCellSettled { cell }
            if cell.cell_id.trim().is_empty() || cell.is_active() || cell.finished_at.is_none() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "command-cell terminal fact must have a settled typed state".to_string(),
            ))
        }
        ChatDriverEvent::AwaiterResultReady { result }
            if result.receipt.execution_id.trim().is_empty()
                || result.receipt.cell_id != result.cell.cell_id
                || result.cell.is_active() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "Awaiter Ready fact requires exact receipt identity and terminal cell truth"
                    .to_string(),
            ))
        }
        ChatDriverEvent::AwaiterResultDeliveryStarted { acknowledgement }
        | ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement }
            if acknowledgement.execution_id.trim().is_empty()
                || acknowledgement.acknowledged_turn_id.trim().is_empty() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "Awaiter acknowledgement identity is incomplete".to_string(),
            ))
        }
        ChatDriverEvent::ExtensionReceipt(receipt)
            if receipt.meta().request_id.trim().is_empty()
                || receipt.meta().operation_id.trim().is_empty() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "Extension receipt identity is incomplete".to_string(),
            ))
        }
        _ => Ok(()),
    }
}

fn has_duplicate_ids(input_ids: &[String]) -> bool {
    let mut seen = HashSet::with_capacity(input_ids.len());
    input_ids.iter().any(|input_id| !seen.insert(input_id))
}

fn stable_event_id(stream_id: &str, sequence: u64, content_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CHAT_EVENT_SCHEMA_VERSION.to_be_bytes());
    hasher.update(stream_id.as_bytes());
    hasher.update(sequence.to_be_bytes());
    hasher.update(content_hash.as_bytes());
    format!("chat_evt_{:x}", hasher.finalize())
}

fn digest(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    format!("sha256_{:x}", hasher.finalize())
}

#[derive(Serialize)]
struct EnvelopeIntegrity<'a> {
    schema_version: u16,
    sequence: u64,
    stream_id: &'a str,
    workspace_id: &'a str,
    conversation_id: Option<&'a str>,
    root_turn_id: &'a str,
    turn_id: &'a str,
    message_id: &'a str,
    timestamp: DateTime<Utc>,
    payload: &'a ChatDriverEvent,
}

fn envelope_content_hash(integrity: EnvelopeIntegrity<'_>) -> Result<String, ChatEventLogError> {
    let encoded = echo_agent::utils::canonical_json::canonical_json_bytes(&integrity)
        .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
    Ok(digest(&encoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::command_cells::{
        AwaiterResult, AwaiterResultAcknowledgement, AwaiterSummaryStatus, AwaiterWatchReceipt,
        AwaiterWatchState,
    };
    use crate::tasks::task_runtime::types::{
        BackgroundCellArtifactStatus, BackgroundCellPhase, BackgroundCellState,
        BackgroundCellTerminalCause,
    };
    use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity, ToolInvocation};
    use echo_agent::tools::ToolResult;

    #[derive(Default)]
    struct CapturingSink {
        journaled: Mutex<Vec<ChatEventEnvelope>>,
    }

    impl crate::chat_driver::ChatSink for CapturingSink {
        fn on_event(&self, _event: ChatDriverEvent) -> bool {
            false
        }

        fn on_journaled_event(&self, envelope: ChatEventEnvelope) -> bool {
            self.journaled
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(envelope);
            true
        }
    }

    fn agent_event(turn: &str, sequence: u64, text: &str) -> Result<ChatDriverEvent, String> {
        let identity =
            EventIdentity::for_chat(Some("conversation-1".to_string()), turn, turn, None)
                .map_err(|error| error.to_string())?;
        EventEnvelope::new(
            &identity,
            sequence,
            None,
            AgentEvent::Token(text.to_string()),
        )
        .map(|event| ChatDriverEvent::Agent(Box::new(event)))
        .map_err(|error| error.to_string())
    }

    fn append_status(log: &ChatEventLog, status: &str) -> Result<(), String> {
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::TurnStatus {
                status: status.to_string(),
            },
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    fn segment_count(log: &ChatEventLog) -> Result<usize, String> {
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let cached = log
            .stream_journal(&stream, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stream is missing".to_string())?;
        let guard = lock_cached_stream(&cached);
        let authority = guard
            .as_ref()
            .ok_or_else(|| "stream authority is missing".to_string())?;
        Ok(authority.journal.segments().len())
    }

    fn recovered_pin_records(log: &ChatEventLog) -> Result<usize, String> {
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let cached = log
            .stream_journal(&stream, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stream is missing".to_string())?;
        let guard = lock_cached_stream(&cached);
        Ok(guard
            .as_ref()
            .ok_or_else(|| "stream authority is missing".to_string())?
            .pins
            .recovered_records)
    }

    fn awaiter_result() -> AwaiterResult {
        let now = Utc::now();
        AwaiterResult {
            receipt: AwaiterWatchReceipt {
                execution_id: "await-execution".to_string(),
                control_task_id: "awaiter:cell:1".to_string(),
                attempt: 1,
                watch_generation: 1,
                cell_id: "cell".to_string(),
                workspace_id: "workspace-1".to_string(),
                conversation_id: "conversation-1".to_string(),
                run_id: None,
                root_turn_id: "turn-1".to_string(),
                state: AwaiterWatchState::Settled,
                started_at: now,
                settled_at: Some(now),
            },
            cell: BackgroundCellState {
                cell_id: "cell".to_string(),
                name: "test".to_string(),
                command_hash: "sha256:test".to_string(),
                turn_id: Some("turn-1".to_string()),
                execution_id: Some("cell-execution".to_string()),
                call_id: Some("call".to_string()),
                phase: BackgroundCellPhase::Succeeded,
                terminal_cause: Some(BackgroundCellTerminalCause::Exited),
                terminal_message: None,
                exit_code: Some(0),
                artifact_status: BackgroundCellArtifactStatus::BelowThreshold,
                artifact_message: None,
                total_output_bytes: 2,
                output_truncated: false,
                output_excerpt: Some("ok".to_string()),
                artifact_path: None,
                artifact_sha256: None,
                started_at: now,
                finished_at: Some(now),
            },
            awaiter_status: AwaiterSummaryStatus::Completed,
            awaiter_summary: Some("done".to_string()),
        }
    }

    fn active_chat_cell(cell_id: &str) -> BackgroundCellState {
        let mut cell = awaiter_result().cell;
        cell.cell_id = cell_id.to_string();
        cell.phase = BackgroundCellPhase::Running;
        cell.terminal_cause = None;
        cell.exit_code = None;
        cell.artifact_status = BackgroundCellArtifactStatus::Writing;
        cell.finished_at = None;
        cell
    }

    #[test]
    fn boot_recovery_closes_ordinary_chat_orphan_once() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("chat-events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::CommandCellStarted {
                cell: Box::new(active_chat_cell("orphan-chat-cell")),
            },
        )
        .map_err(|error| error.to_string())?;
        drop(log);

        let restarted = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        assert_eq!(
            restarted
                .recover_orphan_command_cells()
                .map_err(|error| error.to_string())?,
            1
        );
        assert_eq!(
            restarted
                .recover_orphan_command_cells()
                .map_err(|error| error.to_string())?,
            0
        );
        let replay = restarted
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        let settled = replay
            .events
            .iter()
            .filter_map(|event| match &event.payload {
                ChatDriverEvent::CommandCellSettled { cell }
                    if cell.cell_id == "orphan-chat-cell" =>
                {
                    Some(cell.as_ref())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(settled.len(), 1);
        let cell = settled
            .first()
            .ok_or_else(|| "recovered Chat cell missing".to_string())?;
        assert_eq!(cell.phase, BackgroundCellPhase::Failed);
        assert_eq!(
            cell.terminal_cause,
            Some(BackgroundCellTerminalCause::Interrupted)
        );
        assert_eq!(cell.artifact_status, BackgroundCellArtifactStatus::Failed);
        Ok(())
    }

    #[test]
    fn live_terminal_wins_orphan_recovery_without_duplicate_terminal() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let pause = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        let log = Arc::new(
            ChatEventLog::open(
                temp.path().join("chat-events"),
                ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?
            .with_orphan_recovery_pause(Arc::clone(&pause)),
        );
        let started = active_chat_cell("racing-chat-cell");
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::CommandCellStarted {
                cell: Box::new(started.clone()),
            },
        )
        .map_err(|error| error.to_string())?;
        let recovering = Arc::clone(&log);
        let recovery = std::thread::spawn(move || recovering.recover_orphan_command_cells());
        pause.0.wait();

        let mut terminal = started;
        terminal.phase = BackgroundCellPhase::Succeeded;
        terminal.terminal_cause = Some(BackgroundCellTerminalCause::Exited);
        terminal.exit_code = Some(0);
        terminal.artifact_status = BackgroundCellArtifactStatus::BelowThreshold;
        terminal.finished_at = Some(Utc::now());
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::CommandCellSettled {
                cell: Box::new(terminal),
            },
        )
        .map_err(|error| error.to_string())?;
        pause.1.wait();
        assert_eq!(
            recovery
                .join()
                .map_err(|_| "orphan recovery thread panicked".to_string())?
                .map_err(|error| error.to_string())?,
            0
        );
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        let terminals = replay
            .events
            .iter()
            .filter_map(|event| match &event.payload {
                ChatDriverEvent::CommandCellSettled { cell }
                    if cell.cell_id == "racing-chat-cell" =>
                {
                    Some(cell.phase)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(terminals, vec![BackgroundCellPhase::Succeeded]);
        Ok(())
    }

    #[test]
    fn rust_wire_model_losslessly_accepts_frontend_fixture() -> Result<(), String> {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../web-frontend/src/fixtures/chat-event-envelope-v4.json"
        ));
        let expected: serde_json::Value =
            serde_json::from_str(fixture).map_err(|error| error.to_string())?;
        let envelopes: Vec<ChatEventEnvelope> =
            serde_json::from_value(expected.clone()).map_err(|error| error.to_string())?;
        assert_eq!(
            serde_json::to_value(envelopes).map_err(|error| error.to_string())?,
            expected
        );
        Ok(())
    }

    #[test]
    fn typed_round_trip_preserves_framework_envelope_and_cursor() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            agent_event("turn-1", 7, "你好")?,
        )
        .map_err(|error| error.to_string())?;
        append_status(&log, "completed")?;
        drop(log);

        let reopened = ChatEventLog::open(root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let replay = reopened
            .replay("workspace-1", Some("conversation-1"), "ignored", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.latest_cursor, 2);
        let first = replay
            .events
            .first()
            .ok_or_else(|| "missing event".to_string())?;
        assert!(
            matches!(&first.payload, ChatDriverEvent::Agent(agent) if matches!(&agent.payload, AgentEvent::Token(text) if text == "你好"))
        );
        Ok(())
    }

    #[test]
    fn replay_cap_and_retained_gap_have_distinct_cursors() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(
            temp.path(),
            ChatEventRetention {
                segment_rollover_bytes: 1,
                max_segments: 2,
                max_replay_events: 2,
            },
        )
        .map_err(|error| error.to_string())?;
        for sequence in 1..=4 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", sequence, "delta")?,
            )
            .map_err(|error| error.to_string())?;
        }
        append_status(&log, "completed")?;
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.truncated);
        assert_eq!(replay.retained_earliest_cursor, Some(4));
        assert_eq!(replay.returned_earliest_cursor, Some(4));
        assert_eq!(replay.latest_cursor, 5);
        assert_eq!(replay.events.len(), 2);
        Ok(())
    }

    #[test]
    fn queued_input_pin_ignores_public_cap_then_converges_on_remove() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(
            temp.path(),
            ChatEventRetention {
                segment_rollover_bytes: 1,
                max_segments: 1,
                max_replay_events: 1,
            },
        )
        .map_err(|error| error.to_string())?;
        log.enqueue_chat_input(
            "workspace-1",
            "conversation-1",
            "input-1",
            "keep me".to_string(),
            Vec::new(),
        )
        .map_err(|error| error.to_string())?;
        for _ in 0..4 {
            append_status(&log, "completed")?;
        }
        assert_eq!(
            log.queued_chat_inputs("workspace-1", "conversation-1")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert!(segment_count(&log)? > 1);
        log.remove_queued_chat_input("workspace-1", "conversation-1", "input-1")
            .map_err(|error| error.to_string())?;
        assert!(
            log.queued_chat_inputs("workspace-1", "conversation-1")
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert_eq!(segment_count(&log)?, 1);
        Ok(())
    }

    #[test]
    fn unacknowledged_awaiter_pins_then_acknowledgement_converges() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(
            temp.path(),
            ChatEventRetention {
                segment_rollover_bytes: 1,
                max_segments: 1,
                max_replay_events: 1,
            },
        )
        .map_err(|error| error.to_string())?;
        let result = awaiter_result();
        let event = || ChatDriverEvent::AwaiterResultReady {
            result: Box::new(result.clone()),
        };
        let first = log
            .append("workspace-1", Some("conversation-1"), "turn-1", event())
            .map_err(|error| error.to_string())?;
        let duplicate = log
            .append("workspace-1", Some("conversation-1"), "turn-1", event())
            .map_err(|error| error.to_string())?;
        assert_eq!(first.event_id, duplicate.event_id);
        for _ in 0..3 {
            append_status(&log, "completed")?;
        }
        assert!(segment_count(&log)? > 1);
        assert_eq!(
            log.pending_awaiter_results("workspace-1", "conversation-1", "turn-1")
                .map_err(|error| error.to_string())?,
            vec![result.clone()]
        );
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::AwaiterResultAcknowledged {
                acknowledgement: AwaiterResultAcknowledgement {
                    execution_id: result.receipt.execution_id,
                    attempt: result.receipt.attempt,
                    watch_generation: result.receipt.watch_generation,
                    cell_id: result.receipt.cell_id,
                    acknowledged_turn_id: "next-turn".to_string(),
                    outcome:
                        crate::tasks::task_runtime::command_cells::AwaiterDeliveryOutcome::Drained,
                },
            },
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(segment_count(&log)?, 1);
        Ok(())
    }

    #[test]
    fn every_surface_journals_before_render_and_projects_tools() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path().join("events"), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let tools = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tools"))
                .map_err(|error| error.to_string())?,
        );
        for (offset, surface) in [
            ChatSurface::Gui,
            ChatSurface::Tui,
            ChatSurface::Cli,
            ChatSurface::Channel,
        ]
        .into_iter()
        .enumerate()
        {
            let captured = Arc::new(CapturingSink::default());
            let turn = format!("turn-{offset}");
            let sink = bind_surface_chat_sink(
                surface,
                captured.clone(),
                log.clone(),
                tools.clone(),
                "workspace-1",
                Some("conversation-1".to_string()),
                &turn,
            );
            assert!(sink.on_event(ChatDriverEvent::TurnStatus {
                status: "running".to_string(),
            }));
            let identity =
                EventIdentity::for_chat(Some("conversation-1".to_string()), &turn, &turn, None)
                    .map_err(|error| error.to_string())?;
            let call_id = format!("call-{offset}");
            let call = EventEnvelope::new(
                &identity,
                1,
                None,
                AgentEvent::ToolCall {
                    call_id: call_id.clone(),
                    invocation: ToolInvocation {
                        requested_name: "shell".to_string(),
                        requested_args: serde_json::json!({"command": "requested"}),
                        name: "sandbox_shell".to_string(),
                        args: serde_json::json!({"command": "effective"}),
                        rewrites: Vec::new(),
                    },
                },
            )
            .map_err(|error| error.to_string())?;
            assert!(sink.on_event(ChatDriverEvent::Agent(Box::new(call))));
            let result = EventEnvelope::new(
                &identity,
                2,
                None,
                AgentEvent::ToolResult {
                    call_id,
                    name: "sandbox_shell".to_string(),
                    result: ToolResult::success("done"),
                },
            )
            .map_err(|error| error.to_string())?;
            assert!(sink.on_event(ChatDriverEvent::Agent(Box::new(result))));
            assert_eq!(
                captured
                    .journaled
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .len(),
                3
            );
        }
        assert_eq!(
            log.replay("workspace-1", Some("conversation-1"), "ignored", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            12
        );
        assert_eq!(
            tools
                .summaries_for_conversation("workspace-1", "conversation-1")
                .len(),
            4
        );
        Ok(())
    }

    #[test]
    fn scoped_deletion_releases_authorities_and_preserves_other_workspace() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for workspace in ["workspace-1", "workspace-2"] {
            log.append(
                workspace,
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        log.remove_conversation("workspace-1", "conversation-1")
            .map_err(|error| error.to_string())?;
        assert!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );
        assert_eq!(
            log.replay("workspace-2", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        log.remove_workspace("workspace-2")
            .map_err(|error| error.to_string())?;
        assert!(
            log.replay("workspace-2", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn replaced_root_and_stream_symlinks_fail_closed() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        append_status(&log, "completed")?;
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let stream_dir = log.stream_dir(&stream);
        let backup = temp.path().join("stream-backup");
        fs::rename(&stream_dir, &backup).map_err(|error| error.to_string())?;
        symlink(&outside, &stream_dir).map_err(|error| error.to_string())?;
        assert!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .is_err()
        );
        fs::remove_file(&stream_dir).map_err(|error| error.to_string())?;
        fs::rename(&backup, &stream_dir).map_err(|error| error.to_string())?;
        let root_backup = temp.path().join("root-backup");
        fs::rename(&root, &root_backup).map_err(|error| error.to_string())?;
        symlink(&outside, &root).map_err(|error| error.to_string())?;
        assert!(append_status(&log, "completed").is_err());
        Ok(())
    }

    #[test]
    fn durability_policy_preserves_delta_and_safe_point_classes() -> Result<(), String> {
        assert_eq!(
            append_durability(&agent_event("turn-1", 1, "delta")?),
            FileDurability::Flush
        );
        assert_eq!(
            append_durability(&ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            }),
            FileDurability::SyncData
        );
        assert_eq!(
            append_durability(&ChatDriverEvent::InputQueued {
                input_id: "input".to_string(),
                text: "queued".to_string(),
                attachments: Vec::new(),
                submitted_at_ms: 1,
            }),
            FileDurability::SyncData
        );
        assert!(should_maintain_retention(
            FileDurability::SyncData,
            &JournalDurabilityStatus::Confirmed,
        ));
        assert!(!should_maintain_retention(
            FileDurability::Flush,
            &JournalDurabilityStatus::Confirmed,
        ));
        assert!(!should_maintain_retention(
            FileDurability::SyncData,
            &JournalDurabilityStatus::Degraded {
                error: "barrier failed after the full record committed".to_string(),
            },
        ));
        assert!(should_mark_barrier_pending(
            FileDurability::SyncData,
            &JournalDurabilityStatus::Degraded {
                error: "one committed sequence still owes a barrier".to_string(),
            },
        ));
        assert!(!should_mark_barrier_pending(
            FileDurability::SyncData,
            &JournalDurabilityStatus::Confirmed,
        ));
        Ok(())
    }

    #[test]
    fn outer_content_hash_is_stable_across_unordered_payload_maps() -> Result<(), String> {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../web-frontend/src/fixtures/chat-event-envelope-v4.json"
        ));
        let mut fixture: serde_json::Value =
            serde_json::from_str(fixture).map_err(|error| error.to_string())?;
        let metadata = fixture
            .pointer_mut("/1/payload/event/payload/data/result/metadata")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| "tool-result metadata fixture missing".to_string())?;
        for key in ["zeta", "alpha", "gamma", "beta"] {
            metadata.insert(key.to_string(), serde_json::Value::String(key.to_string()));
        }
        let payload = fixture
            .pointer("/1/payload")
            .cloned()
            .ok_or_else(|| "tool-result payload fixture missing".to_string())?;
        let first: ChatDriverEvent =
            serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
        let second: ChatDriverEvent =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        let timestamp = DateTime::parse_from_rfc3339("2026-08-16T00:00:01Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);
        let hash = |payload: &ChatDriverEvent| {
            envelope_content_hash(EnvelopeIntegrity {
                schema_version: CHAT_EVENT_SCHEMA_VERSION,
                sequence: 2,
                stream_id: r#"["workspace-1","fixture-conversation"]"#,
                workspace_id: "workspace-1",
                conversation_id: Some("fixture-conversation"),
                root_turn_id: "fixture-message",
                turn_id: "fixture-turn",
                message_id: "fixture-message",
                timestamp,
                payload,
            })
        };
        assert_eq!(
            hash(&first).map_err(|error| error.to_string())?,
            hash(&second).map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[test]
    fn workspace_isolation_and_cross_conversation_rejection_hold() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for workspace in ["workspace-a", "workspace-b"] {
            log.append(
                workspace,
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", 1, workspace)?,
            )
            .map_err(|error| error.to_string())?;
        }
        assert_eq!(
            log.replay("workspace-a", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        assert_eq!(
            log.replay("workspace-b", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        assert!(matches!(
            log.append(
                "workspace-a",
                Some("conversation-2"),
                "turn-1",
                agent_event("turn-1", 2, "wrong conversation")?,
            ),
            Err(ChatEventLogError::InvalidIdentity(_))
        ));
        Ok(())
    }

    #[test]
    fn invalid_nested_schema_and_turn_status_fail_on_append_and_replay() -> Result<(), String> {
        for invalid_kind in ["framework_schema", "turn_status"] {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?;
            let mut invalid = if invalid_kind == "framework_schema" {
                agent_event("turn-1", 1, "invalid")?
            } else {
                ChatDriverEvent::TurnStatus {
                    status: "future_terminal".to_string(),
                }
            };
            if let ChatDriverEvent::Agent(envelope) = &mut invalid {
                envelope.schema_version =
                    echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION.saturating_add(1);
            }
            assert!(matches!(
                log.append("workspace-1", Some("conversation-1"), "turn-1", invalid,),
                Err(ChatEventLogError::InvalidEvent(_))
            ));

            let persisted_payload = if invalid_kind == "framework_schema" {
                let mut event = agent_event("turn-1", 1, "invalid replay")?;
                if let ChatDriverEvent::Agent(envelope) = &mut event {
                    envelope.schema_version =
                        echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION.saturating_add(1);
                }
                event
            } else {
                ChatDriverEvent::TurnStatus {
                    status: "future_terminal".to_string(),
                }
            };
            let selected = stream_id("workspace-1", Some("conversation-1"), "turn-1")
                .map_err(|error| error.to_string())?;
            let cached = log
                .stream_journal(&selected, true)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "stream missing".to_string())?;
            let guard = lock_cached_stream(&cached);
            guard
                .as_ref()
                .ok_or_else(|| "authority missing".to_string())?
                .journal
                .append(PersistedChatEvent {
                    schema_version: CHAT_EVENT_SCHEMA_VERSION,
                    stream_id: selected,
                    workspace_id: "workspace-1".to_string(),
                    conversation_id: Some("conversation-1".to_string()),
                    root_turn_id: "turn-1".to_string(),
                    turn_id: "turn-1".to_string(),
                    message_id: "turn-1".to_string(),
                    timestamp: Utc::now(),
                    payload: persisted_payload,
                })
                .map_err(|error| error.to_string())?;
            drop(guard);
            assert!(matches!(
                log.replay("workspace-1", Some("conversation-1"), "turn-1", 0),
                Err(ChatEventLogError::Corrupt { .. })
            ));
            drop(cached);
            drop(log);
            let reopened = ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?;
            assert!(matches!(
                reopened.append(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-1",
                    ChatDriverEvent::TurnStatus {
                        status: "running".to_string(),
                    },
                ),
                Err(ChatEventLogError::Corrupt { .. })
            ));
        }
        Ok(())
    }

    #[test]
    fn persistence_failure_never_reaches_renderer() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        fs::remove_dir(&root).map_err(|error| error.to_string())?;
        fs::write(&root, b"not a directory").map_err(|error| error.to_string())?;
        let captured = Arc::new(CapturingSink::default());
        let tools = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tools"))
                .map_err(|error| error.to_string())?,
        );
        let sink = bind_surface_chat_sink(
            ChatSurface::Gui,
            captured.clone(),
            log,
            tools,
            "workspace-1",
            Some("conversation-1".to_string()),
            "turn-1",
        );
        assert!(!sink.on_event(ChatDriverEvent::TurnStatus {
            status: "running".to_string(),
        }));
        assert!(
            captured
                .journaled
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn one_locked_stream_does_not_block_another_conversation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        log.append(
            "workspace-1",
            Some("blocked"),
            "blocked-turn",
            ChatDriverEvent::TurnStatus {
                status: "running".to_string(),
            },
        )
        .map_err(|error| error.to_string())?;
        let blocked_id = stream_id("workspace-1", Some("blocked"), "blocked-turn")
            .map_err(|error| error.to_string())?;
        let blocked = log
            .stream_journal(&blocked_id, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "blocked stream missing".to_string())?;
        let blocked_guard = lock_cached_stream(&blocked);
        let free_log = Arc::clone(&log);
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = free_log
                .append(
                    "workspace-1",
                    Some("free"),
                    "free-turn",
                    ChatDriverEvent::TurnStatus {
                        status: "running".to_string(),
                    },
                )
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| format!("independent stream blocked: {error}"))??;
        drop(blocked_guard);
        handle
            .join()
            .map_err(|_| "independent stream thread failed".to_string())?;
        Ok(())
    }

    #[test]
    fn queued_inputs_survive_reopen_reorder_and_scoped_removal() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for (input_id, text) in [("input-a", "first"), ("input-b", "second")] {
            log.enqueue_chat_input(
                "workspace-a",
                "conversation-1",
                input_id,
                text.to_string(),
                Vec::new(),
            )
            .map_err(|error| error.to_string())?;
        }
        log.enqueue_chat_input(
            "workspace-b",
            "conversation-1",
            "other",
            "other workspace".to_string(),
            Vec::new(),
        )
        .map_err(|error| error.to_string())?;
        log.reorder_queued_chat_inputs(
            "workspace-a",
            "conversation-1",
            vec!["input-b".to_string(), "input-a".to_string()],
        )
        .map_err(|error| error.to_string())?;
        log.remove_queued_chat_input("workspace-a", "conversation-1", "input-b")
            .map_err(|error| error.to_string())?;
        log.enqueue_chat_input(
            "workspace-a",
            "conversation-1",
            "input-b",
            "requeued at tail".to_string(),
            Vec::new(),
        )
        .map_err(|error| error.to_string())?;
        assert!(matches!(
            log.reorder_queued_chat_inputs(
                "workspace-a",
                "conversation-1",
                vec!["input-a".to_string(), "input-a".to_string()],
            ),
            Err(ChatEventLogError::InvalidEvent(_))
        ));
        drop(log);

        let reopened = ChatEventLog::open(root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .queued_chat_inputs("workspace-a", "conversation-1")
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|input| input.input_id)
                .collect::<Vec<_>>(),
            vec!["input-a".to_string(), "input-b".to_string()]
        );
        assert_eq!(
            reopened
                .queued_chat_inputs("workspace-b", "conversation-1")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn incremental_pin_projection_does_not_rescan_pinned_history() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let retention = ChatEventRetention {
            segment_rollover_bytes: 1,
            max_segments: 1,
            max_replay_events: 1,
        };
        let log = ChatEventLog::open(&root, retention).map_err(|error| error.to_string())?;
        log.enqueue_chat_input(
            "workspace-1",
            "conversation-1",
            "input-pin",
            "pinned".to_string(),
            Vec::new(),
        )
        .map_err(|error| error.to_string())?;
        let result = awaiter_result();
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::AwaiterResultReady {
                result: Box::new(result),
            },
        )
        .map_err(|error| error.to_string())?;
        for _ in 0..4 {
            append_status(&log, "completed")?;
        }
        drop(log);

        let reopened = ChatEventLog::open(root, retention).map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .queued_chat_inputs("workspace-1", "conversation-1")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert_eq!(
            reopened
                .pending_awaiter_results("workspace-1", "conversation-1", "turn-1")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        let recovered_once = recovered_pin_records(&reopened)?;
        assert!(recovered_once >= 6);
        for _ in 0..12 {
            append_status(&reopened, "completed")?;
            assert_eq!(
                reopened
                    .queued_chat_inputs("workspace-1", "conversation-1")
                    .map_err(|error| error.to_string())?
                    .len(),
                1
            );
            assert_eq!(
                reopened
                    .pending_awaiter_results("workspace-1", "conversation-1", "turn-1")
                    .map_err(|error| error.to_string())?
                    .len(),
                1
            );
        }
        assert_eq!(recovered_pin_records(&reopened)?, recovered_once);
        Ok(())
    }

    #[test]
    fn two_handles_share_pins_idempotency_deletion_and_recreation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let first = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let second = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        first
            .enqueue_chat_input(
                "workspace-1",
                "conversation-1",
                "input-shared",
                "shared".to_string(),
                Vec::new(),
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(
            second
                .queued_chat_inputs("workspace-1", "conversation-1")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        let mismatched = ChatEventLog::open(
            &root,
            ChatEventRetention {
                segment_rollover_bytes: 1,
                ..ChatEventRetention::default()
            },
        )
        .map_err(|error| error.to_string())?;
        assert!(matches!(
            mismatched.queued_chat_inputs("workspace-1", "conversation-1"),
            Err(ChatEventLogError::Corrupt { .. })
        ));

        let result = awaiter_result();
        let ready = || ChatDriverEvent::AwaiterResultReady {
            result: Box::new(result.clone()),
        };
        let original = first
            .append("workspace-1", Some("conversation-1"), "turn-1", ready())
            .map_err(|error| error.to_string())?;
        let duplicate = second
            .append("workspace-1", Some("conversation-1"), "turn-1", ready())
            .map_err(|error| error.to_string())?;
        assert_eq!(original.event_id, duplicate.event_id);
        let mut conflicting = result;
        conflicting.awaiter_summary = Some("conflict".to_string());
        assert!(matches!(
            second.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::AwaiterResultReady {
                    result: Box::new(conflicting),
                },
            ),
            Err(ChatEventLogError::InvalidEvent(_))
        ));

        first
            .remove_conversation("workspace-1", "conversation-1")
            .map_err(|error| error.to_string())?;
        assert!(
            second
                .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );
        second
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-new",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(
            first
                .replay("workspace-1", Some("conversation-1"), "turn-new", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn deletion_holds_shared_lifecycle_barrier_against_reopen() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let pause = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        let deleting = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?
                .with_deletion_pause(Arc::clone(&pause)),
        );
        let other = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        deleting
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        let deletion_log = Arc::clone(&deleting);
        let deletion = std::thread::spawn(move || {
            deletion_log
                .remove_conversation("workspace-1", "conversation-1")
                .map_err(|error| error.to_string())
        });
        pause.0.wait();
        let reopen_log = Arc::clone(&other);
        let (tx, rx) = std::sync::mpsc::channel();
        let reopen = std::thread::spawn(move || {
            let result = reopen_log
                .append(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-race",
                    ChatDriverEvent::TurnStatus {
                        status: "running".to_string(),
                    },
                )
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        pause.1.wait();
        deletion
            .join()
            .map_err(|_| "deletion thread failed".to_string())??;
        let raced = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| error.to_string())?;
        reopen
            .join()
            .map_err(|_| "reopen thread failed".to_string())?;
        if raced.is_err() {
            other
                .append(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-after-delete",
                    ChatDriverEvent::TurnStatus {
                        status: "completed".to_string(),
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        assert_eq!(
            other
                .replay(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-after-delete",
                    0
                )
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn direct_conversation_deletion_ignores_unrelated_corrupt_stream() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        append_status(&log, "completed")?;
        let unrelated = root.join("sha256_unrelated_corrupt_stream");
        fs::create_dir_all(&unrelated).map_err(|error| error.to_string())?;
        fs::write(
            unrelated.join("00000000000000000001.jsonl"),
            b"not a framework journal record\n",
        )
        .map_err(|error| error.to_string())?;
        log.remove_conversation("workspace-1", "conversation-1")
            .map_err(|error| error.to_string())?;
        assert!(unrelated.exists());
        Ok(())
    }

    #[test]
    fn swapped_real_stream_directories_fail_selected_identity_validation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for conversation in ["conversation-a", "conversation-b"] {
            log.append(
                "workspace-1",
                Some(conversation),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        let a = log.stream_dir(
            &stream_id("workspace-1", Some("conversation-a"), "turn-1")
                .map_err(|error| error.to_string())?,
        );
        let b = log.stream_dir(
            &stream_id("workspace-1", Some("conversation-b"), "turn-1")
                .map_err(|error| error.to_string())?,
        );
        let swap = root.join("swap");
        fs::rename(&a, &swap).map_err(|error| error.to_string())?;
        fs::rename(&b, &a).map_err(|error| error.to_string())?;
        fs::rename(&swap, &b).map_err(|error| error.to_string())?;

        assert!(matches!(
            log.append(
                "workspace-1",
                Some("conversation-a"),
                "turn-2",
                ChatDriverEvent::TurnStatus {
                    status: "running".to_string(),
                },
            ),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn two_handle_lru_bounds_strong_caches_and_recovers_evicted_pins() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let first = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let second = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        first
            .enqueue_chat_input(
                "workspace-lru",
                "conversation-0",
                "pinned-input",
                "survives eviction".to_string(),
                Vec::new(),
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(
            second
                .queued_chat_inputs("workspace-lru", "conversation-0")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        for index in 1..=(MAX_REGISTRY_ENTRIES_BEFORE_PRUNE + 16) {
            let conversation = format!("conversation-{index}");
            first
                .append(
                    "workspace-lru",
                    Some(&conversation),
                    "turn",
                    ChatDriverEvent::TurnStatus {
                        status: "completed".to_string(),
                    },
                )
                .map_err(|error| error.to_string())?;
            second
                .replay("workspace-lru", Some(&conversation), "turn", 0)
                .map_err(|error| error.to_string())?;
        }
        assert!(first.streams.len() <= MAX_CACHED_STREAMS);
        assert!(second.streams.len() <= MAX_CACHED_STREAMS);
        let canonical_root = fs::canonicalize(&root).map_err(|error| error.to_string())?;
        let registered_for_root = stream_authority_registry()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .filter(|path| path.starts_with(&canonical_root))
            .count();
        assert!(registered_for_root <= MAX_REGISTRY_ENTRIES_BEFORE_PRUNE + 1);
        let pinned_stream = stream_id("workspace-lru", Some("conversation-0"), "pinned-input")
            .map_err(|error| error.to_string())?;
        assert!(!first.streams.contains_key(&pinned_stream));
        assert!(!second.streams.contains_key(&pinned_stream));
        assert_eq!(
            first
                .queued_chat_inputs("workspace-lru", "conversation-0")
                .map_err(|error| error.to_string())?
                .first()
                .map(|input| input.text.as_str()),
            Some("survives eviction")
        );
        Ok(())
    }

    #[test]
    fn pending_barrier_debt_is_not_evicted_under_cache_pressure() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        append_status(&log, "completed")?;
        let protected = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let cached = log
            .stream_journal(&protected, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "protected stream missing".to_string())?;
        {
            let mut guard = lock_cached_stream(&cached);
            guard
                .as_mut()
                .ok_or_else(|| "protected authority missing".to_string())?
                .barrier_pending = true;
        }
        drop(cached);
        for index in 0..=(MAX_CACHED_STREAMS + 8) {
            let conversation = format!("pressure-{index}");
            log.append(
                "workspace-1",
                Some(&conversation),
                "turn",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        assert!(log.streams.contains_key(&protected));
        let cached = log
            .streams
            .get(&protected)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| "protected cache entry missing".to_string())?;
        lock_cached_stream(&cached)
            .as_mut()
            .ok_or_else(|| "protected authority missing".to_string())?
            .barrier_pending = false;
        drop(cached);
        for index in 0..=(MAX_CACHED_STREAMS + 8) {
            let conversation = format!("confirmed-pressure-{index}");
            log.append(
                "workspace-1",
                Some(&conversation),
                "turn",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        assert!(log.streams.len() <= MAX_CACHED_STREAMS);
        assert!(!log.streams.contains_key(&protected));
        Ok(())
    }

    #[test]
    fn concurrent_first_open_across_two_handles_assigns_one_exact_sequence() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let first = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let second = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let start = Arc::new(std::sync::Barrier::new(33));
        let mut handles = Vec::new();
        for index in 0..32 {
            let log = if index % 2 == 0 {
                Arc::clone(&first)
            } else {
                Arc::clone(&second)
            };
            let start = Arc::clone(&start);
            handles.push(std::thread::spawn(move || {
                start.wait();
                if index < 16 {
                    let input_id = format!("input-{index}");
                    log.append(
                        "workspace-1",
                        Some("conversation-1"),
                        &input_id,
                        ChatDriverEvent::InputQueued {
                            input_id: input_id.clone(),
                            text: format!("queued-{index}"),
                            attachments: Vec::new(),
                            submitted_at_ms: u64::try_from(index).unwrap_or(u64::MAX),
                        },
                    )
                } else {
                    let mut result = awaiter_result();
                    result.receipt.execution_id = format!("awaiter-{index}");
                    result.receipt.control_task_id = format!("awaiter:cell-{index}:1");
                    result.receipt.cell_id = format!("cell-{index}");
                    result.cell.cell_id = format!("cell-{index}");
                    log.append(
                        "workspace-1",
                        Some("conversation-1"),
                        &format!("root-{index}"),
                        ChatDriverEvent::AwaiterResultReady {
                            result: Box::new(result),
                        },
                    )
                }
                .map_err(|error| error.to_string())
            }));
        }
        start.wait();
        let mut envelopes = Vec::new();
        for handle in handles {
            envelopes.push(
                handle
                    .join()
                    .map_err(|_| "concurrent append thread failed".to_string())??,
            );
        }
        let mut sequences = envelopes
            .iter()
            .map(|envelope| envelope.sequence)
            .collect::<Vec<_>>();
        sequences.sort_unstable();
        assert_eq!(sequences, (1_u64..=32).collect::<Vec<_>>());
        assert_eq!(
            envelopes
                .iter()
                .map(|envelope| envelope.event_id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            32
        );
        assert_eq!(
            first
                .queued_chat_inputs("workspace-1", "conversation-1")
                .map_err(|error| error.to_string())?
                .len(),
            16
        );
        assert_eq!(
            second
                .pending_awaiter_results("workspace-1", "conversation-1", "ignored")
                .map_err(|error| error.to_string())?
                .len(),
            16
        );
        Ok(())
    }
}
