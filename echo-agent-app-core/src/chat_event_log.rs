//! Application-owned ordered event journal for ordinary chat turns.
//!
//! Formal work continues to use `TaskRuntimeStore`. This log owns only the
//! lossless product stream consumed by chat surfaces, including the framework
//! `EventEnvelope` nested in `ChatDriverEvent::Agent`.
//! Retention is bounded independently per conversation/message stream; the
//! collection of streams is intentionally not described as a global size cap.

use crate::chat_driver::ChatDriverEvent;
use crate::tool_execution::ToolExecutionRepository;
use crate::tool_execution_projection::ToolExecutionProjector;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use echo_core::utils::fs::FileDurability;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs;
#[cfg(test)]
use std::fs::OpenOptions;
#[cfg(test)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard};

pub const CHAT_EVENT_SCHEMA_VERSION: u16 = 2;
const SEGMENT_SUFFIX: &str = ".jsonl";

#[derive(Debug, Clone, Copy)]
pub struct ChatEventRetention {
    /// Per-stream rollover threshold checked before the next append. One
    /// indivisible JSONL record may make the active segment exceed it.
    pub segment_rollover_bytes: u64,
    /// Maximum retained segments after a semantic safe point, independently
    /// for each chat stream. An active unsynced segment may temporarily sit
    /// beside this bounded committed history until the next safe point.
    pub max_segments: usize,
    /// Maximum events returned by one replay response for one stream.
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
    /// Monotonic application cursor within one workspace/conversation stream,
    /// or one workspace/root-message stream when no conversation exists.
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
    /// Earliest cursor still present after segment retention.
    pub retained_earliest_cursor: Option<u64>,
    /// Earliest cursor returned by this bounded response.
    pub returned_earliest_cursor: Option<u64>,
    pub latest_cursor: u64,
    /// True when retention or the replay cap omitted events after the requested cursor.
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

#[derive(Debug, Default)]
struct StreamState {
    initialized: bool,
    last_sequence: u64,
    active_start: u64,
    active_bytes: u64,
    active_unsynced: bool,
    needs_prune: bool,
}

pub struct ChatEventLog {
    root: PathBuf,
    retention: ChatEventRetention,
    streams: DashMap<String, Arc<Mutex<StreamState>>>,
    append_file: Arc<AppendFile>,
}

type AppendFile =
    dyn Fn(&Path, &[u8], FileDurability) -> std::io::Result<()> + Send + Sync + 'static;

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

/// Shared sink decorator used by GUI, TUI, CLI and channel surfaces.
/// The journal append is the delivery boundary: streaming deltas are flushed
/// in order, while semantic boundaries sync all preceding writes before the
/// inner renderer observes them.
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

/// Bind a concrete product surface to the one application-owned ordinary-chat
/// authority. All GUI/TUI/CLI/channel production entry points call this
/// function, so persistence behavior cannot drift between renderers.
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
                            tracing::error!(
                                surface = ?self.surface,
                                "failed to deliver persisted tool-execution projection; closing surface stream"
                            );
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
}

impl ChatEventLog {
    pub fn default_root() -> PathBuf {
        echo_agent::paths::user_data_path("chat-events")
    }

    /// Create the process-wide authority without performing fallible I/O.
    /// The first append/replay creates or validates the selected root and
    /// fails closed if it is unavailable.
    pub fn at_default_root() -> Self {
        Self {
            root: Self::default_root(),
            retention: ChatEventRetention::default(),
            streams: DashMap::new(),
            append_file: Arc::new(echo_core::utils::fs::append_existing),
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
            append_file: Arc::new(echo_core::utils::fs::append_existing),
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
        let stream_dir = self.stream_dir(&selected_stream_id);
        let stream_state = self.stream_state(&selected_stream_id);
        let mut state = lock_stream_state(&stream_state);
        ensure_real_directory(&self.root, true)?;
        ensure_real_directory(&stream_dir, true)?;
        if !state.initialized {
            self.initialize_stream(&selected_stream_id, &stream_dir, &mut state)?;
        }

        if let Some(fact_key) = awaiter_fact_key(&event) {
            for (start, path) in list_segments(&stream_dir)? {
                let scan = scan_segment(&path, &selected_stream_id, false, start)?;
                for existing in scan.events {
                    if awaiter_fact_key(&existing.payload).as_deref() != Some(fact_key.as_str()) {
                        continue;
                    }
                    let expected =
                        echo_core::utils::canonical_json::canonical_json_bytes(&event)
                            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
                    let actual =
                        echo_core::utils::canonical_json::canonical_json_bytes(&existing.payload)
                            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
                    return if expected == actual {
                        Ok(existing)
                    } else {
                        Err(ChatEventLogError::InvalidEvent(format!(
                            "conflicting Awaiter fact for {fact_key}"
                        )))
                    };
                }
            }
        }

        let sequence =
            state
                .last_sequence
                .checked_add(1)
                .ok_or_else(|| ChatEventLogError::Corrupt {
                    path: stream_dir.clone(),
                    message: "chat event sequence exhausted".to_string(),
                })?;
        let rolled = state.active_start == 0
            || (state.active_bytes >= self.retention.segment_rollover_bytes
                && state.last_sequence >= state.active_start);
        if rolled {
            self.sync_active_segment_before_roll(&stream_dir, &mut state)?;
            self.prune_segments_to(
                &selected_stream_id,
                &stream_dir,
                self.retention.max_segments,
            )?;
            self.roll_segment(&stream_dir, &mut state, sequence)?;
        }

        let timestamp = Utc::now();
        let content_hash = envelope_content_hash(EnvelopeIntegrity {
            schema_version: CHAT_EVENT_SCHEMA_VERSION,
            sequence,
            stream_id: &selected_stream_id,
            workspace_id,
            conversation_id,
            root_turn_id,
            turn_id: &turn_id,
            message_id: &message_id,
            timestamp,
            payload: &event,
        })?;
        let event_id = stable_event_id(&selected_stream_id, sequence, &content_hash);
        let envelope = ChatEventEnvelope {
            schema_version: CHAT_EVENT_SCHEMA_VERSION,
            event_id,
            content_hash,
            sequence,
            stream_id: selected_stream_id,
            workspace_id: workspace_id.to_string(),
            conversation_id: conversation_id.map(ToString::to_string),
            root_turn_id: root_turn_id.to_string(),
            turn_id,
            message_id,
            timestamp,
            payload: event,
        };
        let mut encoded = serde_json::to_vec(&envelope)
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        encoded.push(b'\n');
        let active_path = segment_path(&stream_dir, state.active_start);
        let durability = append_durability(&envelope.payload);
        if let Err(source) = (self.append_file)(&active_path, &encoded, durability) {
            // A failed write may still have changed the file. Force the next
            // append through the canonical scan/repair path before assigning
            // another sequence number.
            state.initialized = false;
            return Err(ChatEventLogError::Io {
                path: active_path,
                source,
            });
        }
        state.last_sequence = sequence;
        state.active_bytes = state
            .active_bytes
            .saturating_add(u64::try_from(encoded.len()).unwrap_or(u64::MAX));
        state.active_unsynced = matches!(durability, FileDurability::Flush);
        // A streaming rollover may retain one extra active segment so an
        // unsynced delta never replaces the newest committed history. The next
        // semantic safe point syncs that active segment and restores the exact
        // per-stream retention cap.
        state.needs_prune = rolled || state.needs_prune;
        if matches!(durability, FileDurability::SyncData) && state.needs_prune {
            match self.prune_segments_to(
                &envelope.stream_id,
                &stream_dir,
                self.retention.max_segments,
            ) {
                Ok(()) => state.needs_prune = false,
                Err(error) => {
                    // The envelope and every preceding delta are already
                    // synced. Retention is a derived maintenance action: it
                    // must remain retryable instead of turning a committed
                    // terminal fact into an apparent append failure.
                    tracing::warn!(
                        %error,
                        stream_id = %envelope.stream_id,
                        sequence = envelope.sequence,
                        "chat event retention remains pending after a committed safe point"
                    );
                }
            }
        }
        Ok(envelope)
    }

    pub fn replay(
        &self,
        workspace_id: &str,
        conversation_id: Option<&str>,
        turn_id: &str,
        after_cursor: u64,
    ) -> Result<ChatEventReplay, ChatEventLogError> {
        let stream_id = stream_id(workspace_id, conversation_id, turn_id)?;
        let stream_dir = self.stream_dir(&stream_id);
        let stream_state = self.stream_state(&stream_id);
        let mut state = lock_stream_state(&stream_state);
        if !ensure_real_directory(&self.root, false)? {
            *state = StreamState::default();
            return Ok(ChatEventReplay {
                events: Vec::new(),
                retained_earliest_cursor: None,
                returned_earliest_cursor: None,
                latest_cursor: 0,
                truncated: false,
            });
        }
        if !ensure_real_directory(&stream_dir, false)? {
            *state = StreamState::default();
            return Ok(ChatEventReplay {
                events: Vec::new(),
                retained_earliest_cursor: None,
                returned_earliest_cursor: None,
                latest_cursor: 0,
                truncated: false,
            });
        }
        self.initialize_stream(&stream_id, &stream_dir, &mut state)?;
        let segments = list_segments(&stream_dir)?;
        let mut replay = VecDeque::new();
        let mut retained_earliest_cursor = None;
        let mut latest_cursor = 0_u64;
        let mut capped = false;
        for (start, path) in segments {
            let scan = scan_segment(&path, &stream_id, false, start)?;
            for event in scan.events {
                retained_earliest_cursor.get_or_insert(event.sequence);
                latest_cursor = event.sequence;
                if event.sequence <= after_cursor {
                    continue;
                }
                if replay.len() == self.retention.max_replay_events {
                    replay.pop_front();
                    capped = true;
                }
                replay.push_back(event);
            }
        }
        let retained_gap = retained_earliest_cursor
            .and_then(|cursor| cursor.checked_sub(1))
            .is_some_and(|before_earliest| after_cursor < before_earliest);
        let returned_earliest_cursor = replay.front().map(|event| event.sequence);
        Ok(ChatEventReplay {
            events: replay.into_iter().collect(),
            retained_earliest_cursor,
            returned_earliest_cursor,
            latest_cursor,
            truncated: retained_gap || capped,
        })
    }

    pub fn pending_awaiter_results(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        root_turn_id: &str,
    ) -> Result<Vec<crate::tasks::task_runtime::command_cells::AwaiterResult>, ChatEventLogError>
    {
        let selected_stream_id = stream_id(workspace_id, Some(conversation_id), root_turn_id)?;
        let stream_dir = self.stream_dir(&selected_stream_id);
        let stream_state = self.stream_state(&selected_stream_id);
        let mut state = lock_stream_state(&stream_state);
        if !ensure_real_directory(&self.root, false)? || !ensure_real_directory(&stream_dir, false)?
        {
            *state = StreamState::default();
            return Ok(Vec::new());
        }
        self.initialize_stream(&selected_stream_id, &stream_dir, &mut state)?;
        let mut pending = std::collections::BTreeMap::<
            String,
            crate::tasks::task_runtime::command_cells::AwaiterResult,
        >::new();
        for (start, path) in list_segments(&stream_dir)? {
            let scan = scan_segment(&path, &selected_stream_id, false, start)?;
            for event in scan.events {
                match event.payload {
                    ChatDriverEvent::AwaiterResultReady { result } => {
                        pending.insert(awaiter_receipt_key(&result.receipt), *result);
                    }
                    ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement } => {
                        pending.remove(&awaiter_ack_key(&acknowledgement));
                    }
                    _ => {}
                }
            }
        }
        Ok(pending.into_values().collect())
    }

    pub fn pending_awaiter_results_for_conversation(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<Vec<crate::tasks::task_runtime::command_cells::AwaiterResult>, ChatEventLogError>
    {
        if !ensure_real_directory(&self.root, false)? {
            return Ok(Vec::new());
        }
        let streams = self.conversation_streams(workspace_id, conversation_id)?;
        let mut pending = Vec::new();
        for (_, stream_dir) in streams {
            let Some(first) = first_stream_envelope(&stream_dir)? else {
                continue;
            };
            pending.extend(self.pending_awaiter_results(
                workspace_id,
                conversation_id,
                &first.root_turn_id,
            )?);
        }
        pending.sort_by(|left, right| {
            left.receipt
                .started_at
                .cmp(&right.receipt.started_at)
                .then_with(|| left.receipt.execution_id.cmp(&right.receipt.execution_id))
        });
        Ok(pending)
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
        let replay = self.replay(workspace_id, Some(conversation_id), conversation_id, 0)?;
        let mut order = Vec::new();
        let mut queued = std::collections::HashMap::<String, QueuedChatInput>::new();
        for envelope in replay.events {
            match envelope.payload {
                ChatDriverEvent::InputQueued {
                    input_id,
                    text,
                    attachments,
                    submitted_at_ms,
                } => {
                    if !queued.contains_key(&input_id) {
                        order.push(input_id.clone());
                    }
                    queued.insert(
                        input_id.clone(),
                        QueuedChatInput {
                            input_id,
                            workspace_id: workspace_id.to_string(),
                            conversation_id: conversation_id.to_string(),
                            text,
                            attachments,
                            submitted_at_ms,
                        },
                    );
                }
                ChatDriverEvent::InputRemoved { input_id } => {
                    queued.remove(&input_id);
                }
                ChatDriverEvent::InputReordered { input_ids } => {
                    let mut next = input_ids
                        .into_iter()
                        .filter(|input_id| queued.contains_key(input_id))
                        .collect::<Vec<_>>();
                    let remaining = order
                        .iter()
                        .filter(|input_id| queued.contains_key(*input_id))
                        .filter(|input_id| !next.contains(input_id))
                        .cloned()
                        .collect::<Vec<_>>();
                    next.extend(remaining);
                    order = next;
                }
                _ => {}
            }
        }
        Ok(order
            .into_iter()
            .filter_map(|input_id| queued.remove(&input_id))
            .collect())
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
        if input_ids.is_empty() || input_ids.iter().any(|input_id| input_id.trim().is_empty()) {
            return Err(ChatEventLogError::InvalidEvent(
                "queued input order must contain non-empty identities".to_string(),
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

    /// Remove the ordinary-chat journal for one conversation. Append and
    /// replay take the same per-stream lock, so callers that have suspended
    /// foreground admission cannot observe or recreate a partially removed
    /// stream. Message-only streams are intentionally outside this scope.
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
        for (stream_id, stream_dir) in self.conversation_streams(workspace_id, conversation_id)? {
            let stream_state = self.stream_state(&stream_id);
            let mut state = lock_stream_state(&stream_state);
            fs::remove_dir_all(&stream_dir).map_err(|source| ChatEventLogError::Io {
                path: stream_dir,
                source,
            })?;
            *state = StreamState::default();
            self.streams.remove(&stream_id);
        }
        Ok(())
    }

    /// Remove every ordinary-chat and message-only stream owned by one workspace.
    /// Workspace deletion holds application admission before calling this method,
    /// so removed streams cannot be recreated during the sweep.
    pub fn remove_workspace(&self, workspace_id: &str) -> Result<(), ChatEventLogError> {
        if workspace_id.trim().is_empty() {
            return Err(ChatEventLogError::InvalidIdentity(
                "workspace_id must not be empty".to_string(),
            ));
        }
        if !ensure_real_directory(&self.root, false)? {
            return Ok(());
        }
        let entries = fs::read_dir(&self.root).map_err(|source| ChatEventLogError::Io {
            path: self.root.clone(),
            source,
        })?;
        let mut matches = Vec::new();
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
                return Err(ChatEventLogError::Corrupt {
                    path,
                    message: "chat event stream must not be a symlink".to_string(),
                });
            }
            if !metadata.is_dir() {
                continue;
            }
            if let Some(envelope) = first_stream_envelope(&path)?
                && envelope.workspace_id == workspace_id
            {
                matches.push((envelope.stream_id, path));
            }
        }
        for (stream_id, stream_dir) in matches {
            let stream_state = self.stream_state(&stream_id);
            let mut state = lock_stream_state(&stream_state);
            fs::remove_dir_all(&stream_dir).map_err(|source| ChatEventLogError::Io {
                path: stream_dir,
                source,
            })?;
            *state = StreamState::default();
            self.streams.remove(&stream_id);
        }
        Ok(())
    }

    fn conversation_streams(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<Vec<(String, PathBuf)>, ChatEventLogError> {
        let mut matches = Vec::new();
        let entries = fs::read_dir(&self.root).map_err(|source| ChatEventLogError::Io {
            path: self.root.clone(),
            source,
        })?;
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
                return Err(ChatEventLogError::Corrupt {
                    path,
                    message: "chat event stream must not be a symlink".to_string(),
                });
            }
            if !metadata.is_dir() {
                continue;
            }
            let Some(envelope) = first_stream_envelope(&path)? else {
                continue;
            };
            if envelope.workspace_id == workspace_id
                && envelope.conversation_id.as_deref() == Some(conversation_id)
            {
                matches.push((envelope.stream_id, path));
            }
        }
        Ok(matches)
    }

    fn initialize_stream(
        &self,
        stream_id: &str,
        stream_dir: &Path,
        state: &mut StreamState,
    ) -> Result<(), ChatEventLogError> {
        let segments = list_segments(stream_dir)?;
        let mut expected = segments.first().map(|(start, _)| *start).unwrap_or(1);
        let mut active_start = 0_u64;
        let mut active_bytes = 0_u64;
        for (position, (start, path)) in segments.iter().enumerate() {
            if *start != expected {
                return Err(ChatEventLogError::Corrupt {
                    path: path.clone(),
                    message: format!("segment starts at {start}, expected {expected}"),
                });
            }
            let is_latest = position.checked_add(1) == Some(segments.len());
            let scan = scan_segment(path, stream_id, is_latest, *start)?;
            if scan.first_sequence != Some(*start) && !(is_latest && scan.events.is_empty()) {
                return Err(ChatEventLogError::Corrupt {
                    path: path.clone(),
                    message: "segment filename does not match first event".to_string(),
                });
            }
            if let Some(last) = scan.last_sequence {
                expected = last
                    .checked_add(1)
                    .ok_or_else(|| ChatEventLogError::Corrupt {
                        path: path.clone(),
                        message: "chat event sequence exhausted".to_string(),
                    })?;
                state.last_sequence = last;
            } else if is_latest {
                // Rollover durably creates the next segment before append. If
                // the following append tears and repairs to empty, its filename
                // remains the cursor boundary until a successful retry prunes
                // older segments.
                state.last_sequence =
                    start
                        .checked_sub(1)
                        .ok_or_else(|| ChatEventLogError::Corrupt {
                            path: path.clone(),
                            message: "empty segment cannot start at sequence zero".to_string(),
                        })?;
            }
            active_start = *start;
            active_bytes = scan.bytes;
        }
        state.active_start = active_start;
        state.active_bytes = active_bytes;
        // A fresh process already survived any prior crash. Marking a nonempty
        // latest segment conservatively dirty also covers an in-process append
        // that wrote a full record before returning an I/O error.
        state.active_unsynced = active_bytes > 0;
        state.needs_prune = segments.len() > self.retention.max_segments;
        state.initialized = true;
        Ok(())
    }

    fn roll_segment(
        &self,
        stream_dir: &Path,
        state: &mut StreamState,
        next_sequence: u64,
    ) -> Result<(), ChatEventLogError> {
        let next_path = segment_path(stream_dir, next_sequence);
        echo_core::utils::fs::atomic_write(&next_path, b"").map_err(|source| {
            ChatEventLogError::Io {
                path: next_path,
                source,
            }
        })?;
        state.active_start = next_sequence;
        state.active_bytes = 0;
        state.active_unsynced = false;
        Ok(())
    }

    fn sync_active_segment_before_roll(
        &self,
        stream_dir: &Path,
        state: &mut StreamState,
    ) -> Result<(), ChatEventLogError> {
        if state.active_start == 0 || !state.active_unsynced {
            return Ok(());
        }
        let active_path = segment_path(stream_dir, state.active_start);
        (self.append_file)(&active_path, b"", FileDurability::SyncData).map_err(|source| {
            ChatEventLogError::Io {
                path: active_path,
                source,
            }
        })?;
        state.active_unsynced = false;
        Ok(())
    }

    fn prune_segments_to(
        &self,
        stream_id: &str,
        stream_dir: &Path,
        retained_segments: usize,
    ) -> Result<(), ChatEventLogError> {
        let segments = list_segments(stream_dir)?;
        let mut pending_ready_segments = std::collections::HashMap::<String, u64>::new();
        for (start, path) in &segments {
            let scan = scan_segment(path, stream_id, false, *start)?;
            for event in scan.events {
                match &event.payload {
                    ChatDriverEvent::AwaiterResultReady { result } => {
                        pending_ready_segments.insert(awaiter_receipt_key(&result.receipt), *start);
                    }
                    ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement } => {
                        pending_ready_segments.remove(&awaiter_ack_key(acknowledgement));
                    }
                    _ => {}
                }
            }
        }
        let pinned = pending_ready_segments
            .into_values()
            .collect::<std::collections::HashSet<_>>();
        let mut remaining = segments.len();
        for (start, path) in &segments {
            if remaining <= retained_segments {
                break;
            }
            if pinned.contains(start) {
                break;
            }
            fs::remove_file(path).map_err(|source| ChatEventLogError::Io {
                path: path.clone(),
                source,
            })?;
            remaining = remaining.saturating_sub(1);
        }
        Ok(())
    }

    fn stream_dir(&self, stream_id: &str) -> PathBuf {
        self.root.join(digest(stream_id.as_bytes()))
    }

    fn stream_state(&self, stream_id: &str) -> Arc<Mutex<StreamState>> {
        let entry = self
            .streams
            .entry(stream_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(StreamState::default())));
        Arc::clone(entry.value())
    }

    #[cfg(test)]
    fn with_append_file(mut self, append_file: Arc<AppendFile>) -> Self {
        self.append_file = append_file;
        self
    }
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
        | ChatDriverEvent::Interrupt { .. }
        | ChatDriverEvent::CommandCellStarted { .. }
        | ChatDriverEvent::CommandCellSettled { .. }
        | ChatDriverEvent::AwaiterResultReady { .. }
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

fn lock_stream_state(stream: &Mutex<StreamState>) -> MutexGuard<'_, StreamState> {
    stream.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("chat event stream lock was poisoned; recovering state");
        poisoned.into_inner()
    })
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
        return Err(ChatEventLogError::Corrupt {
            path: path.to_path_buf(),
            message: "chat event directory path is not a real directory".to_string(),
        });
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
        ChatDriverEvent::AwaiterResultReady { result } => {
            Some(format!("ready:{}", awaiter_receipt_key(&result.receipt)))
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
        && !is_known_turn_status(status)
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
        && (input_ids.is_empty() || input_ids.iter().any(|input_id| input_id.trim().is_empty()))
    {
        return Err(ChatEventLogError::InvalidEvent(
            "queued input order contains an empty identity".to_string(),
        ));
    }
    match event {
        ChatDriverEvent::CommandCellStarted { cell }
            if cell.cell_id.trim().is_empty() || !cell.is_active() =>
        {
            return Err(ChatEventLogError::InvalidEvent(
                "command-cell Started fact must have an active typed state".to_string(),
            ));
        }
        ChatDriverEvent::CommandCellSettled { cell }
            if cell.cell_id.trim().is_empty() || cell.is_active() || cell.finished_at.is_none() =>
        {
            return Err(ChatEventLogError::InvalidEvent(
                "command-cell terminal fact must have a settled typed state".to_string(),
            ));
        }
        ChatDriverEvent::AwaiterResultReady { result }
            if result.receipt.execution_id.trim().is_empty()
                || result.receipt.cell_id != result.cell.cell_id
                || result.cell.is_active() =>
        {
            return Err(ChatEventLogError::InvalidEvent(
                "Awaiter Ready fact requires exact receipt identity and terminal cell truth"
                    .to_string(),
            ));
        }
        ChatDriverEvent::AwaiterResultAcknowledged { acknowledgement }
            if acknowledgement.execution_id.trim().is_empty()
                || acknowledgement.acknowledged_turn_id.trim().is_empty() =>
        {
            return Err(ChatEventLogError::InvalidEvent(
                "Awaiter acknowledgement identity is incomplete".to_string(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn is_known_turn_status(status: &str) -> bool {
    matches!(
        status,
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
    // Integrity must validate after restart, not only in-process. The shared
    // encoder recursively sorts nested maps such as ToolResult metadata.
    let encoded = echo_core::utils::canonical_json::canonical_json_bytes(&integrity)
        .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
    Ok(digest(&encoded))
}

fn segment_path(stream_dir: &Path, start: u64) -> PathBuf {
    stream_dir.join(format!("{start:020}{SEGMENT_SUFFIX}"))
}

fn list_segments(stream_dir: &Path) -> Result<Vec<(u64, PathBuf)>, ChatEventLogError> {
    let mut segments = Vec::new();
    let entries = fs::read_dir(stream_dir).map_err(|source| ChatEventLogError::Io {
        path: stream_dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ChatEventLogError::Io {
            path: stream_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| ChatEventLogError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ChatEventLogError::Corrupt {
                path,
                message: "chat event segment must not be a symlink".to_string(),
            });
        }
        if !metadata.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(start) = name.strip_suffix(SEGMENT_SUFFIX) else {
            continue;
        };
        let start = start
            .parse::<u64>()
            .map_err(|error| ChatEventLogError::Corrupt {
                path: path.clone(),
                message: format!("invalid segment filename: {error}"),
            })?;
        segments.push((start, path));
    }
    segments.sort_by_key(|(start, _)| *start);
    Ok(segments)
}

fn first_stream_envelope(
    stream_dir: &Path,
) -> Result<Option<ChatEventEnvelope>, ChatEventLogError> {
    let Some((_, path)) = list_segments(stream_dir)?.into_iter().next() else {
        return Ok(None);
    };
    let bytes =
        echo_core::utils::fs::read_existing(&path).map_err(|source| ChatEventLogError::Io {
            path: path.clone(),
            source,
        })?;
    let Some(line) = bytes.split(|byte| *byte == b'\n').next() else {
        return Ok(None);
    };
    if line.is_empty() {
        return Ok(None);
    }
    let raw = serde_json::from_slice::<serde_json::Value>(line).map_err(|error| {
        ChatEventLogError::Corrupt {
            path: path.clone(),
            message: format!("invalid first chat event record: {error}"),
        }
    })?;
    if raw
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(u64::from(CHAT_EVENT_SCHEMA_VERSION))
    {
        return Ok(None);
    }
    serde_json::from_value(raw)
        .map(Some)
        .map_err(|error| ChatEventLogError::Corrupt {
            path,
            message: format!("invalid first chat event envelope: {error}"),
        })
}

struct SegmentScan {
    events: Vec<ChatEventEnvelope>,
    first_sequence: Option<u64>,
    last_sequence: Option<u64>,
    bytes: u64,
}

fn scan_segment(
    path: &Path,
    stream_id: &str,
    repair_torn_tail: bool,
    expected_start: u64,
) -> Result<SegmentScan, ChatEventLogError> {
    let bytes =
        echo_core::utils::fs::read_existing(path).map_err(|source| ChatEventLogError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut events = Vec::new();
    let mut valid_bytes = 0_usize;
    let mut previous = None;
    let mut repaired = false;
    for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
        let terminated = chunk.last().is_some_and(|byte| *byte == b'\n');
        if !terminated && !repair_torn_tail {
            return Err(ChatEventLogError::Corrupt {
                path: path.to_path_buf(),
                message: "immutable segment contains an unterminated JSONL record".to_string(),
            });
        }
        let line = if terminated {
            chunk.strip_suffix(b"\n").unwrap_or(chunk)
        } else {
            chunk
        };
        if line.is_empty() {
            return Err(ChatEventLogError::Corrupt {
                path: path.to_path_buf(),
                message: "blank JSONL record".to_string(),
            });
        }
        let raw = match serde_json::from_slice::<serde_json::Value>(line) {
            Ok(raw) => raw,
            Err(error) if repair_torn_tail && !terminated => {
                echo_core::utils::fs::truncate_existing(
                    path,
                    u64::try_from(valid_bytes).unwrap_or(u64::MAX),
                    FileDurability::SyncData,
                )
                .map_err(|source| ChatEventLogError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                repaired = true;
                tracing::warn!(path = %path.display(), %error, "repaired torn chat event tail");
                break;
            }
            Err(error) => {
                return Err(ChatEventLogError::Corrupt {
                    path: path.to_path_buf(),
                    message: format!("invalid JSONL record: {error}"),
                });
            }
        };
        let event = serde_json::from_value::<ChatEventEnvelope>(raw.clone()).map_err(|error| {
            ChatEventLogError::Corrupt {
                path: path.to_path_buf(),
                message: format!("invalid chat event envelope: {error}"),
            }
        })?;
        let typed = serde_json::to_value(&event).map_err(|error| ChatEventLogError::Corrupt {
            path: path.to_path_buf(),
            message: format!("chat event envelope could not be normalized: {error}"),
        })?;
        if typed != raw {
            return Err(ChatEventLogError::Corrupt {
                path: path.to_path_buf(),
                message: "chat event envelope contains fields not preserved by the current schema"
                    .to_string(),
            });
        }
        validate_envelope(path, stream_id, previous, expected_start, &event)?;
        previous = Some(event.sequence);
        events.push(event);
        valid_bytes = valid_bytes.saturating_add(chunk.len());
        if !terminated && repair_torn_tail {
            echo_core::utils::fs::append_existing(path, b"\n", FileDurability::SyncData).map_err(
                |source| ChatEventLogError::Io {
                    path: path.to_path_buf(),
                    source,
                },
            )?;
            valid_bytes = valid_bytes.saturating_add(1);
            repaired = true;
        }
    }
    let first_sequence = events.first().map(|event| event.sequence);
    let last_sequence = events.last().map(|event| event.sequence);
    let bytes = if repaired {
        u64::try_from(valid_bytes).unwrap_or(u64::MAX)
    } else {
        u64::try_from(bytes.len()).unwrap_or(u64::MAX)
    };
    Ok(SegmentScan {
        events,
        first_sequence,
        last_sequence,
        bytes,
    })
}

fn validate_envelope(
    path: &Path,
    stream_id: &str,
    previous: Option<u64>,
    expected_start: u64,
    event: &ChatEventEnvelope,
) -> Result<(), ChatEventLogError> {
    if event.schema_version != CHAT_EVENT_SCHEMA_VERSION {
        return Err(ChatEventLogError::Corrupt {
            path: path.to_path_buf(),
            message: format!("unsupported schema version {}", event.schema_version),
        });
    }
    validate_driver_event(&event.payload).map_err(|error| ChatEventLogError::Corrupt {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    validate_event_stream_identity(
        &event.workspace_id,
        event.conversation_id.as_deref(),
        &event.payload,
    )
    .map_err(|error| ChatEventLogError::Corrupt {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    if event.stream_id != stream_id {
        return Err(ChatEventLogError::Corrupt {
            path: path.to_path_buf(),
            message: "stream identity mismatch".to_string(),
        });
    }
    let expected_stream_id =
        expected_stream_id_for_envelope(event).map_err(|error| ChatEventLogError::Corrupt {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    if event.stream_id != expected_stream_id {
        return Err(ChatEventLogError::Corrupt {
            path: path.to_path_buf(),
            message: "conversation/message identity does not match stream id".to_string(),
        });
    }
    let (expected_turn_id, expected_message_id) =
        event_identity(&event.payload, &event.root_turn_id);
    if event.turn_id != expected_turn_id
        || event.message_id != expected_message_id
        || event.message_id != event.root_turn_id
    {
        return Err(ChatEventLogError::Corrupt {
            path: path.to_path_buf(),
            message: "outer turn/message identity does not match its payload".to_string(),
        });
    }
    let expected = previous
        .and_then(|sequence| sequence.checked_add(1))
        .unwrap_or(expected_start);
    if event.sequence != expected {
        return Err(ChatEventLogError::Corrupt {
            path: path.to_path_buf(),
            message: format!(
                "event sequence {} does not follow {previous:?}",
                event.sequence
            ),
        });
    }
    let content_hash = envelope_content_hash(EnvelopeIntegrity {
        schema_version: CHAT_EVENT_SCHEMA_VERSION,
        sequence: event.sequence,
        stream_id: &event.stream_id,
        workspace_id: &event.workspace_id,
        conversation_id: event.conversation_id.as_deref(),
        root_turn_id: &event.root_turn_id,
        turn_id: &event.turn_id,
        message_id: &event.message_id,
        timestamp: event.timestamp,
        payload: &event.payload,
    })?;
    if event.content_hash != content_hash
        || event.event_id != stable_event_id(stream_id, event.sequence, &content_hash)
    {
        return Err(ChatEventLogError::Corrupt {
            path: path.to_path_buf(),
            message: "event integrity mismatch".to_string(),
        });
    }
    Ok(())
}

fn expected_stream_id_for_envelope(event: &ChatEventEnvelope) -> Result<String, ChatEventLogError> {
    stream_id(
        &event.workspace_id,
        event.conversation_id.as_deref(),
        &event.root_turn_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity, ToolInvocation};
    use echo_agent::tools::ToolResult;

    const TEST_TURN_1_STREAM_ID: &str = r#"["workspace-1","conversation-1"]"#;

    #[derive(Default)]
    struct CapturingSink {
        journaled: Mutex<Vec<ChatEventEnvelope>>,
    }

    impl crate::chat_driver::ChatSink for CapturingSink {
        fn on_event(&self, _event: ChatDriverEvent) -> bool {
            false
        }

        fn on_journaled_event(&self, envelope: ChatEventEnvelope) -> bool {
            lock_captured(&self.journaled).push(envelope);
            true
        }
    }

    fn lock_captured(
        captured: &Mutex<Vec<ChatEventEnvelope>>,
    ) -> MutexGuard<'_, Vec<ChatEventEnvelope>> {
        captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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

    #[test]
    fn rust_wire_model_losslessly_accepts_the_frontend_v4_fixture() -> Result<(), String> {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../web-frontend/src/fixtures/chat-event-envelope-v4.json"
        ));
        let expected: serde_json::Value =
            serde_json::from_str(fixture).map_err(|error| error.to_string())?;
        let envelopes: Vec<ChatEventEnvelope> =
            serde_json::from_value(expected.clone()).map_err(|error| error.to_string())?;
        assert_eq!(envelopes.len(), 2);

        let call = envelopes
            .first()
            .ok_or_else(|| "tool-call fixture missing".to_string())?;
        let ChatDriverEvent::Agent(call) = &call.payload else {
            return Err("fixture did not preserve the framework envelope".to_string());
        };
        let AgentEvent::ToolCall { invocation, .. } = &call.payload else {
            return Err("fixture did not preserve the tool invocation".to_string());
        };
        assert_eq!(invocation.requested_name, "shell");
        assert_eq!(invocation.name, "sandbox_shell");
        assert_eq!(invocation.rewrites.len(), 3);

        let completion = envelopes
            .get(1)
            .ok_or_else(|| "tool-result fixture missing".to_string())?;
        let ChatDriverEvent::Agent(completion) = &completion.payload else {
            return Err("fixture did not preserve the result envelope".to_string());
        };
        let AgentEvent::ToolResult { result, .. } = &completion.payload else {
            return Err("fixture did not preserve the rich tool result".to_string());
        };
        assert!(!result.success);
        assert!(result.truncated);
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.category),
            Some(echo_agent::tools::ToolFailureCategory::Timeout)
        );
        assert_eq!(
            result.metadata.get("artifact_path").map(String::as_str),
            Some("/tmp/tool-output.txt")
        );

        let round_trip = serde_json::to_value(envelopes).map_err(|error| error.to_string())?;
        assert_eq!(round_trip, expected);
        Ok(())
    }

    #[test]
    fn integrity_hash_is_stable_across_unordered_payload_maps() -> Result<(), String> {
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
        for key in ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"] {
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
        let timestamp = || -> Result<DateTime<Utc>, String> {
            Ok(DateTime::parse_from_rfc3339("2026-08-16T00:00:01Z")
                .map_err(|error| error.to_string())?
                .with_timezone(&Utc))
        };
        let first_hash = envelope_content_hash(EnvelopeIntegrity {
            schema_version: CHAT_EVENT_SCHEMA_VERSION,
            sequence: 2,
            stream_id: "[\"workspace-1\",\"fixture-conversation\"]",
            workspace_id: "workspace-1",
            conversation_id: Some("fixture-conversation"),
            root_turn_id: "fixture-message",
            turn_id: "fixture-turn",
            message_id: "fixture-message",
            timestamp: timestamp()?,
            payload: &first,
        })
        .map_err(|error| error.to_string())?;
        let second_hash = envelope_content_hash(EnvelopeIntegrity {
            schema_version: CHAT_EVENT_SCHEMA_VERSION,
            sequence: 2,
            stream_id: "[\"workspace-1\",\"fixture-conversation\"]",
            workspace_id: "workspace-1",
            conversation_id: Some("fixture-conversation"),
            root_turn_id: "fixture-message",
            turn_id: "fixture-turn",
            message_id: "fixture-message",
            timestamp: timestamp()?,
            payload: &second,
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(first_hash, second_hash);
        Ok(())
    }

    #[test]
    fn round_trip_preserves_framework_envelope_and_rebind_cursor() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let first = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", 7, "你")?,
            )
            .map_err(|error| error.to_string())?;
        let second = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);

        let rebound = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let replay = rebound
            .replay(
                "workspace-1",
                Some("conversation-1"),
                "ignored-for-conversation",
                0,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.latest_cursor, 2);
        let first = replay
            .events
            .first()
            .ok_or_else(|| "first replay event missing".to_string())?;
        let ChatDriverEvent::Agent(agent) = &first.payload else {
            return Err("framework envelope was not preserved".to_string());
        };
        assert_eq!(
            agent.schema_version,
            echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION
        );
        assert_eq!(agent.sequence, 7);
        assert_eq!(agent.turn_id.as_str(), "turn-1");
        assert!(matches!(&agent.payload, AgentEvent::Token(value) if value == "你"));
        Ok(())
    }

    #[test]
    fn outer_wire_preserves_agent_turn_and_root_message_identity() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let identity = EventIdentity::for_chat(
            Some("conversation-1".to_string()),
            "continuation-turn",
            "root-message",
            None,
        )
        .map_err(|error| error.to_string())?;
        let event = EventEnvelope::new(
            &identity,
            1,
            None,
            AgentEvent::Token("continued".to_string()),
        )
        .map_err(|error| error.to_string())?;
        let envelope = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "root-message",
                ChatDriverEvent::Agent(Box::new(event)),
            )
            .map_err(|error| error.to_string())?;

        assert_eq!(envelope.turn_id, "continuation-turn");
        assert_eq!(envelope.message_id, "root-message");
        assert_eq!(envelope.workspace_id, "workspace-1");
        assert_eq!(envelope.root_turn_id, "root-message");
        assert_eq!(envelope.stream_id, r#"["workspace-1","conversation-1"]"#);
        Ok(())
    }

    #[test]
    fn identical_conversation_turn_is_isolated_by_workspace() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for workspace_id in ["workspace-a", "workspace-b"] {
            log.append(
                workspace_id,
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", 1, workspace_id)?,
            )
            .map_err(|error| error.to_string())?;
        }

        let workspace_a = log
            .replay("workspace-a", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        let workspace_b = log
            .replay("workspace-b", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(workspace_a.events.len(), 1);
        assert_eq!(workspace_b.events.len(), 1);
        assert_eq!(
            workspace_a
                .events
                .first()
                .map(|event| event.workspace_id.as_str()),
            Some("workspace-a")
        );
        assert_eq!(
            workspace_b
                .events
                .first()
                .map(|event| event.workspace_id.as_str()),
            Some("workspace-b")
        );

        log.remove_conversation("workspace-a", "conversation-1")
            .map_err(|error| error.to_string())?;
        assert!(
            log.replay("workspace-a", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );
        assert_eq!(
            log.replay("workspace-b", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn agent_event_cannot_cross_conversation_journal_streams() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let result = log.append(
            "workspace-1",
            Some("conversation-2"),
            "root-message",
            agent_event("root-message", 1, "wrong stream")?,
        );

        assert!(matches!(result, Err(ChatEventLogError::InvalidIdentity(_))));
        assert!(
            log.replay("workspace-1", Some("conversation-2"), "root-message", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn streaming_tokens_group_commit_at_one_terminal_safe_point() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let sync_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_sync_count = sync_count.clone();
        let append_file: Arc<AppendFile> = Arc::new(move |path, bytes, durability| {
            if matches!(durability, FileDurability::SyncData) {
                observed_sync_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            echo_core::utils::fs::append_existing(path, bytes, durability)
        });
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?
            .with_append_file(append_file);

        for sequence in 1..=128 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", sequence, "delta")?,
            )
            .map_err(|error| error.to_string())?;
        }
        assert_eq!(sync_count.load(std::sync::atomic::Ordering::SeqCst), 0);

        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            },
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(sync_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        let rebound = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let replay = rebound
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.events.len(), 129);
        assert_eq!(replay.latest_cursor, 129);
        Ok(())
    }

    #[test]
    fn streaming_rollover_syncs_closed_delta_segments_before_the_terminal() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let operations = Arc::new(Mutex::new(Vec::new()));
        let observed_operations = operations.clone();
        let append_file: Arc<AppendFile> = Arc::new(move |path, bytes, durability| {
            let durability_name = if matches!(durability, FileDurability::SyncData) {
                "sync"
            } else {
                "flush"
            };
            let record_name = if bytes.is_empty() {
                "barrier"
            } else {
                "record"
            };
            observed_operations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!("{durability_name}:{record_name}"));
            echo_core::utils::fs::append_existing(path, bytes, durability)
        });
        let log = ChatEventLog::open(
            temp.path(),
            ChatEventRetention {
                segment_rollover_bytes: 1,
                max_segments: 1,
                max_replay_events: 16,
            },
        )
        .map_err(|error| error.to_string())?
        .with_append_file(append_file);

        for sequence in 1..=2 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", sequence, "delta")?,
            )
            .map_err(|error| error.to_string())?;
        }
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            },
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(
            operations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .join(","),
            "flush:record,sync:barrier,flush:record,sync:barrier,sync:record"
        );
        assert_eq!(
            list_segments(&log.stream_dir(TEST_TURN_1_STREAM_ID))
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn execution_deltas_flush_until_their_tool_terminal_safe_point() -> Result<(), String> {
        use crate::tasks::task_runtime::executor::ExecEvent;
        use crate::tasks::task_runtime::types::RuntimeEventKind;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let sync_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_sync_count = sync_count.clone();
        let append_file: Arc<AppendFile> = Arc::new(move |path, bytes, durability| {
            if matches!(durability, FileDurability::SyncData) {
                observed_sync_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            echo_core::utils::fs::append_existing(path, bytes, durability)
        });
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?
            .with_append_file(append_file);

        for sequence in 1..=128 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::Execution(ExecEvent::subagent(
                    "workspace-1",
                    "conversation-1",
                    "run-1",
                    "task-1",
                    "subagent-1",
                    RuntimeEventKind::TokenDelta,
                    serde_json::json!({"sequence": sequence, "content": "delta"}),
                )),
            )
            .map_err(|error| error.to_string())?;
        }
        assert_eq!(sync_count.load(std::sync::atomic::Ordering::SeqCst), 0);

        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::Execution(ExecEvent::subagent(
                "workspace-1",
                "conversation-1",
                "run-1",
                "task-1",
                "subagent-1",
                RuntimeEventKind::ToolCompleted,
                serde_json::json!({
                    "call_id": "call-1",
                    "name": "shell",
                    "result": ToolResult::success("done"),
                }),
            )),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(sync_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn append_error_invalidates_state_and_repairs_a_partial_write() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_attempts = attempts.clone();
        let append_file: Arc<AppendFile> = Arc::new(move |path, bytes, durability| {
            if observed_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                let midpoint = bytes.len() / 2;
                let partial = bytes.get(..midpoint).ok_or_else(|| {
                    std::io::Error::other("failed to select partial chat event bytes")
                })?;
                echo_core::utils::fs::append_existing(path, partial, FileDurability::Flush)?;
                return Err(std::io::Error::other("injected append failure"));
            }
            echo_core::utils::fs::append_existing(path, bytes, durability)
        });
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?
            .with_append_file(append_file);

        assert!(
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", 1, "partial")?,
            )
            .is_err()
        );
        let recovered = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(recovered.sequence, 1);
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.events.len(), 1);
        assert_eq!(replay.latest_cursor, 1);
        Ok(())
    }

    #[test]
    fn failed_rollover_append_preserves_committed_history_until_retry_succeeds()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_attempts = attempts.clone();
        let append_file: Arc<AppendFile> = Arc::new(move |path, bytes, durability| {
            let record_attempt = (!bytes.is_empty())
                .then(|| observed_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
            if record_attempt == Some(1) {
                let midpoint = bytes.len() / 2;
                let partial = bytes.get(..midpoint).ok_or_else(|| {
                    std::io::Error::other("failed to select rollover partial bytes")
                })?;
                echo_core::utils::fs::append_existing(path, partial, FileDurability::Flush)?;
                return Err(std::io::Error::other("injected rollover append failure"));
            }
            echo_core::utils::fs::append_existing(path, bytes, durability)
        });
        let log = ChatEventLog::open(
            temp.path(),
            ChatEventRetention {
                segment_rollover_bytes: 1,
                max_segments: 1,
                max_replay_events: 16,
            },
        )
        .map_err(|error| error.to_string())?
        .with_append_file(append_file);

        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            agent_event("turn-1", 1, "first")?,
        )
        .map_err(|error| error.to_string())?;
        assert!(
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", 2, "partial")?,
            )
            .is_err()
        );
        let retained = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(retained.latest_cursor, 1);
        assert_eq!(retained.events.len(), 1);
        assert_eq!(retained.events.first().map(|event| event.sequence), Some(1));
        assert_eq!(
            list_segments(&log.stream_dir(TEST_TURN_1_STREAM_ID))
                .map_err(|error| error.to_string())?
                .len(),
            2
        );
        let recovered = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(recovered.sequence, 2);
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.latest_cursor, 2);
        assert_eq!(replay.events.len(), 1);
        assert_eq!(replay.retained_earliest_cursor, Some(2));
        assert_eq!(
            list_segments(&log.stream_dir(TEST_TURN_1_STREAM_ID))
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn repairs_only_an_incomplete_latest_tail() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            agent_event("turn-1", 1, "ok")?,
        )
        .map_err(|error| error.to_string())?;
        let stream_dir = log.stream_dir(TEST_TURN_1_STREAM_ID);
        let segment = list_segments(&stream_dir)
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .ok_or_else(|| "segment missing".to_string())?;
        OpenOptions::new()
            .append(true)
            .open(&segment)
            .and_then(|mut file| file.write_all(b"{\"schema_version\":"))
            .map_err(|error| error.to_string())?;

        let rebound = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let appended = rebound
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-2",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(appended.sequence, 2);
        assert_eq!(
            rebound
                .replay("workspace-1", Some("conversation-1"), "turn-2", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            2
        );
        Ok(())
    }

    #[test]
    fn unterminated_record_in_an_immutable_segment_fails_closed() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let retention = ChatEventRetention {
            segment_rollover_bytes: 1,
            max_segments: 4,
            max_replay_events: 10,
        };
        let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        for sequence in 1..=2 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", sequence, "ok")?,
            )
            .map_err(|error| error.to_string())?;
        }
        let stream_dir = log.stream_dir(TEST_TURN_1_STREAM_ID);
        let first_segment = list_segments(&stream_dir)
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .ok_or_else(|| "first segment missing".to_string())?;
        let mut bytes = fs::read(&first_segment).map_err(|error| error.to_string())?;
        if bytes.pop() != Some(b'\n') {
            return Err("first segment did not end with a JSONL delimiter".to_string());
        }
        fs::write(&first_segment, bytes).map_err(|error| error.to_string())?;

        let rebound =
            ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        assert!(matches!(
            rebound.replay("workspace-1", Some("conversation-1"), "turn-1", 0),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn complete_corrupt_record_fails_closed() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            agent_event("turn-1", 1, "ok")?,
        )
        .map_err(|error| error.to_string())?;
        let stream_dir = log.stream_dir(TEST_TURN_1_STREAM_ID);
        let segment = list_segments(&stream_dir)
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .ok_or_else(|| "segment missing".to_string())?;
        OpenOptions::new()
            .append(true)
            .open(&segment)
            .and_then(|mut file| file.write_all(b"not-json\n"))
            .map_err(|error| error.to_string())?;
        let rebound = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let error = rebound
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .err()
            .ok_or_else(|| "corrupt record was accepted".to_string())?;
        assert!(matches!(error, ChatEventLogError::Corrupt { .. }));
        Ok(())
    }

    #[test]
    fn complete_unknown_record_without_newline_fails_closed() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            agent_event("turn-1", 1, "ok")?,
        )
        .map_err(|error| error.to_string())?;
        let stream_dir = log.stream_dir(TEST_TURN_1_STREAM_ID);
        let segment = list_segments(&stream_dir)
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .ok_or_else(|| "segment missing".to_string())?;
        OpenOptions::new()
            .append(true)
            .open(&segment)
            .and_then(|mut file| file.write_all(br#"{"source":"future_material_event"}"#))
            .map_err(|error| error.to_string())?;

        let rebound = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let error = rebound
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .err()
            .ok_or_else(|| "unknown complete record was repaired as a torn tail".to_string())?;
        assert!(matches!(error, ChatEventLogError::Corrupt { .. }));
        Ok(())
    }

    #[test]
    fn unknown_envelope_field_fails_closed() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            agent_event("turn-1", 1, "ok")?,
        )
        .map_err(|error| error.to_string())?;
        let segment = list_segments(&log.stream_dir(TEST_TURN_1_STREAM_ID))
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .ok_or_else(|| "segment missing".to_string())?;
        let encoded = fs::read_to_string(&segment).map_err(|error| error.to_string())?;
        let mut envelope: serde_json::Value =
            serde_json::from_str(encoded.trim_end()).map_err(|error| error.to_string())?;
        let object = envelope
            .as_object_mut()
            .ok_or_else(|| "chat envelope is not an object".to_string())?;
        object.insert("future_identity".to_string(), serde_json::json!("unknown"));
        let mut encoded = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
        encoded.push(b'\n');
        fs::write(&segment, encoded).map_err(|error| error.to_string())?;

        let rebound = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            rebound.replay("workspace-1", Some("conversation-1"), "turn-1", 0),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn unknown_nested_framework_field_fails_closed_instead_of_being_dropped() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            agent_event("turn-1", 1, "ok")?,
        )
        .map_err(|error| error.to_string())?;
        let segment = list_segments(&log.stream_dir(TEST_TURN_1_STREAM_ID))
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .ok_or_else(|| "segment missing".to_string())?;
        let encoded = fs::read_to_string(&segment).map_err(|error| error.to_string())?;
        let mut envelope: serde_json::Value =
            serde_json::from_str(encoded.trim_end()).map_err(|error| error.to_string())?;
        let framework_envelope = envelope
            .pointer_mut("/payload/event")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| "nested framework envelope missing".to_string())?;
        framework_envelope.insert(
            "future_framework_identity".to_string(),
            serde_json::json!("must-not-disappear"),
        );
        let mut encoded = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
        encoded.push(b'\n');
        fs::write(&segment, encoded).map_err(|error| error.to_string())?;

        let rebound = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            rebound.replay("workspace-1", Some("conversation-1"), "turn-1", 0),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn unsupported_nested_framework_schema_is_rejected_on_append_and_replay() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let mut unsupported = agent_event("turn-1", 1, "future")?;
        let ChatDriverEvent::Agent(envelope) = &mut unsupported else {
            return Err("expected framework envelope".to_string());
        };
        envelope.schema_version = echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION.saturating_add(1);
        assert!(matches!(
            log.append("workspace-1", Some("conversation-1"), "turn-1", unsupported),
            Err(ChatEventLogError::InvalidEvent(_))
        ));

        let mut envelope = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", 1, "current")?,
            )
            .map_err(|error| error.to_string())?;
        let ChatDriverEvent::Agent(framework) = &mut envelope.payload else {
            return Err("expected framework envelope".to_string());
        };
        framework.schema_version = echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION.saturating_add(1);
        envelope.content_hash = envelope_content_hash(EnvelopeIntegrity {
            schema_version: CHAT_EVENT_SCHEMA_VERSION,
            sequence: envelope.sequence,
            stream_id: &envelope.stream_id,
            workspace_id: &envelope.workspace_id,
            conversation_id: envelope.conversation_id.as_deref(),
            root_turn_id: &envelope.root_turn_id,
            turn_id: &envelope.turn_id,
            message_id: &envelope.message_id,
            timestamp: envelope.timestamp,
            payload: &envelope.payload,
        })
        .map_err(|error| error.to_string())?;
        envelope.event_id = stable_event_id(
            &envelope.stream_id,
            envelope.sequence,
            &envelope.content_hash,
        );
        let segment = list_segments(&log.stream_dir(TEST_TURN_1_STREAM_ID))
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .ok_or_else(|| "segment missing".to_string())?;
        let mut encoded = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
        encoded.push(b'\n');
        fs::write(&segment, encoded).map_err(|error| error.to_string())?;

        let rebound = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            rebound.replay("workspace-1", Some("conversation-1"), "turn-1", 0),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn unknown_turn_status_is_rejected_before_append_and_during_replay() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "future_terminal".to_string(),
                },
            ),
            Err(ChatEventLogError::InvalidEvent(_))
        ));
        assert!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );

        let mut envelope = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        let ChatDriverEvent::TurnStatus { status } = &mut envelope.payload else {
            return Err("expected turn status envelope".to_string());
        };
        *status = "future_terminal".to_string();
        envelope.content_hash = envelope_content_hash(EnvelopeIntegrity {
            schema_version: CHAT_EVENT_SCHEMA_VERSION,
            sequence: envelope.sequence,
            stream_id: &envelope.stream_id,
            workspace_id: &envelope.workspace_id,
            conversation_id: envelope.conversation_id.as_deref(),
            root_turn_id: &envelope.root_turn_id,
            turn_id: &envelope.turn_id,
            message_id: &envelope.message_id,
            timestamp: envelope.timestamp,
            payload: &envelope.payload,
        })
        .map_err(|error| error.to_string())?;
        envelope.event_id = stable_event_id(
            &envelope.stream_id,
            envelope.sequence,
            &envelope.content_hash,
        );
        let segment = list_segments(&log.stream_dir(TEST_TURN_1_STREAM_ID))
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .ok_or_else(|| "segment missing".to_string())?;
        let mut encoded = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
        encoded.push(b'\n');
        fs::write(&segment, encoded).map_err(|error| error.to_string())?;

        let rebound = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            rebound.replay("workspace-1", Some("conversation-1"), "turn-1", 0),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn per_stream_segment_retention_converges_at_safe_point_and_reports_cursor_gap()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let retention = ChatEventRetention {
            segment_rollover_bytes: 1,
            max_segments: 2,
            max_replay_events: 10,
        };
        let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        for sequence in 1..=4 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", sequence, "event")?,
            )
            .map_err(|error| error.to_string())?;
        }
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            },
        )
        .map_err(|error| error.to_string())?;
        let stream_dir = log.stream_dir(TEST_TURN_1_STREAM_ID);
        assert_eq!(
            list_segments(&stream_dir)
                .map_err(|error| error.to_string())?
                .len(),
            2
        );
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.truncated);
        assert_eq!(replay.latest_cursor, 5);
        assert_eq!(replay.retained_earliest_cursor, Some(4));
        assert_eq!(replay.returned_earliest_cursor, Some(4));
        assert_eq!(replay.events.len(), 2);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn retention_failure_after_sync_does_not_hide_the_committed_safe_point() -> Result<(), String> {
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicBool, Ordering};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let external = temp.path().join("external-segment.jsonl");
        fs::write(&external, b"external remains unchanged").map_err(|error| error.to_string())?;
        let sabotage_once = Arc::new(AtomicBool::new(true));
        let sabotage_for_append = Arc::clone(&sabotage_once);
        let replaced_segment = Arc::new(Mutex::new(None::<(PathBuf, PathBuf)>));
        let replaced_for_append = Arc::clone(&replaced_segment);
        let external_for_append = external.clone();
        let append_file: Arc<AppendFile> = Arc::new(move |path, bytes, durability| {
            echo_core::utils::fs::append_existing(path, bytes, durability)?;
            if !bytes.is_empty()
                && matches!(durability, FileDurability::SyncData)
                && sabotage_for_append.swap(false, Ordering::AcqRel)
            {
                let parent = path.parent().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "test segment has no parent",
                    )
                })?;
                let older = list_segments(parent)
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                    .into_iter()
                    .map(|(_, segment)| segment)
                    .find(|segment| segment != path)
                    .ok_or_else(|| std::io::Error::other("test older segment is missing"))?;
                let backup = older.with_extension("backup");
                fs::rename(&older, &backup)?;
                if let Err(error) = symlink(&external_for_append, &older) {
                    let _ = fs::rename(&backup, &older);
                    return Err(error);
                }
                *replaced_for_append
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((older, backup));
            }
            Ok(())
        });
        let log = ChatEventLog::open(
            temp.path(),
            ChatEventRetention {
                segment_rollover_bytes: 1,
                max_segments: 1,
                max_replay_events: 10,
            },
        )
        .map_err(|error| error.to_string())?
        .with_append_file(append_file);
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            agent_event("turn-1", 1, "delta")?,
        )
        .map_err(|error| error.to_string())?;
        let stream_dir = log.stream_dir(TEST_TURN_1_STREAM_ID);

        let committed = log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            },
        );
        let (link, backup) = replaced_segment
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or_else(|| "test did not replace an older segment".to_string())?;
        fs::remove_file(&link).map_err(|error| error.to_string())?;
        fs::rename(&backup, &link).map_err(|error| error.to_string())?;
        let committed = committed.map_err(|error| error.to_string())?;
        assert_eq!(committed.sequence, 2);
        assert_eq!(
            fs::read(&external).map_err(|error| error.to_string())?,
            b"external remains unchanged"
        );
        assert_eq!(
            list_segments(&stream_dir)
                .map_err(|error| error.to_string())?
                .len(),
            2
        );

        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            },
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            list_segments(&stream_dir)
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn replay_cap_distinguishes_retained_and_returned_earliest_cursors() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let retention = ChatEventRetention {
            segment_rollover_bytes: u64::MAX,
            max_segments: 2,
            max_replay_events: 2,
        };
        let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        for sequence in 1..=4 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", sequence, "event")?,
            )
            .map_err(|error| error.to_string())?;
        }

        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.truncated);
        assert_eq!(replay.retained_earliest_cursor, Some(1));
        assert_eq!(replay.returned_earliest_cursor, Some(3));
        assert_eq!(
            replay
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert_eq!(replay.latest_cursor, 4);
        Ok(())
    }

    #[test]
    fn identity_and_timestamp_tampering_fail_closed() -> Result<(), String> {
        for field in ["conversation_id", "turn_id", "message_id", "timestamp"] {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?;
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", 1, "ok")?,
            )
            .map_err(|error| error.to_string())?;
            let stream_dir = log.stream_dir(TEST_TURN_1_STREAM_ID);
            let segment = list_segments(&stream_dir)
                .map_err(|error| error.to_string())?
                .into_iter()
                .next()
                .map(|(_, path)| path)
                .ok_or_else(|| "segment missing".to_string())?;
            let encoded = fs::read_to_string(&segment).map_err(|error| error.to_string())?;
            let mut envelope: ChatEventEnvelope =
                serde_json::from_str(encoded.trim_end()).map_err(|error| error.to_string())?;
            match field {
                "conversation_id" => {
                    envelope.conversation_id = Some("tampered-conversation".to_string());
                }
                "turn_id" => envelope.turn_id = "tampered-turn".to_string(),
                "message_id" => envelope.message_id = "tampered-message".to_string(),
                _ => envelope.timestamp += chrono::Duration::seconds(1),
            }
            let mut tampered = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
            tampered.push(b'\n');
            fs::write(&segment, tampered).map_err(|error| error.to_string())?;

            let rebound = ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?;
            let error = rebound
                .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .err()
                .ok_or_else(|| format!("{field} tampering was accepted"))?;
            assert!(matches!(error, ChatEventLogError::Corrupt { .. }));
        }
        Ok(())
    }

    #[test]
    fn every_product_surface_binds_the_same_group_committed_authority() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let tool_executions = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tools"))
                .map_err(|error| error.to_string())?,
        );
        let surfaces = [
            ChatSurface::Gui,
            ChatSurface::Tui,
            ChatSurface::Cli,
            ChatSurface::Channel,
        ];
        for (offset, surface) in surfaces.into_iter().enumerate() {
            let captured = Arc::new(CapturingSink::default());
            let renderer: Arc<dyn crate::chat_driver::ChatSink> = captured.clone();
            let sink = bind_surface_chat_sink(
                surface,
                renderer,
                log.clone(),
                tool_executions.clone(),
                "workspace-1",
                Some("conversation-1".to_string()),
                format!("turn-{offset}"),
            );
            assert_eq!(
                sink.delivery_guarantee(),
                ChatDeliveryGuarantee::JournaledWithSemanticSafePoints
            );
            assert!(sink.on_event(ChatDriverEvent::TurnStatus {
                status: "running".to_string(),
            }));
            let turn_id = format!("turn-{offset}");
            let identity = EventIdentity::for_chat(
                Some("conversation-1".to_string()),
                &turn_id,
                &turn_id,
                None,
            )
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
                    result: ToolResult::success("complete output"),
                },
            )
            .map_err(|error| error.to_string())?;
            assert!(sink.on_event(ChatDriverEvent::Agent(Box::new(result))));
            let delivered = lock_captured(&captured.journaled);
            assert_eq!(delivered.len(), 3);
        }
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "ignored", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.events.len(), 12);
        let summaries = tool_executions.summaries_for_conversation("workspace-1", "conversation-1");
        assert_eq!(summaries.len(), 4);
        assert!(summaries.iter().all(|summary| {
            summary.status == crate::tool_execution::ToolExecutionStatus::Succeeded
        }));
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
        fs::write(&root, b"not-a-directory").map_err(|error| error.to_string())?;
        let captured = Arc::new(CapturingSink::default());
        let renderer: Arc<dyn crate::chat_driver::ChatSink> = captured.clone();
        let tool_executions = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tools"))
                .map_err(|error| error.to_string())?,
        );
        let sink = bind_surface_chat_sink(
            ChatSurface::Gui,
            renderer,
            log,
            tool_executions,
            "workspace-1",
            Some("conversation-1".to_string()),
            "turn-1",
        );
        assert!(!sink.on_event(ChatDriverEvent::TurnStatus {
            status: "running".to_string(),
        }));
        assert!(lock_captured(&captured.journaled).is_empty());
        Ok(())
    }

    #[test]
    fn a_blocked_stream_does_not_block_another_conversation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let blocked_stream_id = stream_id("workspace-1", Some("blocked"), "turn-blocked")
            .map_err(|error| error.to_string())?;
        let blocked_dir = temp.path().join(digest(blocked_stream_id.as_bytes()));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let wait_for_release = release_rx.clone();
        let append_file: Arc<AppendFile> = Arc::new(move |path, bytes, durability| {
            if path.starts_with(&blocked_dir) {
                entered_tx
                    .send(())
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                wait_for_release
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
            }
            echo_core::utils::fs::append_existing(path, bytes, durability)
        });
        let log = Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?
                .with_append_file(append_file),
        );
        let blocked_log = log.clone();
        let blocked = std::thread::spawn(move || {
            blocked_log
                .append(
                    "workspace-1",
                    Some("blocked"),
                    "turn-blocked",
                    ChatDriverEvent::TurnStatus {
                        status: "running".to_string(),
                    },
                )
                .map_err(|error| error.to_string())
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| format!("blocked stream never entered append: {error}"))?;

        let (free_tx, free_rx) = std::sync::mpsc::channel();
        let free_log = log.clone();
        let free = std::thread::spawn(move || {
            let result = free_log
                .append(
                    "workspace-1",
                    Some("free"),
                    "turn-free",
                    ChatDriverEvent::TurnStatus {
                        status: "running".to_string(),
                    },
                )
                .map_err(|error| error.to_string());
            free_tx.send(result).map_err(|error| error.to_string())
        });
        let free_result = free_rx.recv_timeout(std::time::Duration::from_secs(2));
        let release_result = release_tx.send(()).map_err(|error| error.to_string());
        blocked
            .join()
            .map_err(|_| "blocked stream thread failed".to_string())??;
        free.join()
            .map_err(|_| "free stream thread failed".to_string())??;
        release_result?;
        free_result.map_err(|error| format!("independent stream was blocked: {error}"))??;
        Ok(())
    }

    #[test]
    fn removing_conversation_erases_replay_without_touching_other_streams() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for conversation in ["removed", "retained"] {
            log.append(
                "workspace-1",
                Some(conversation),
                "turn",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }

        log.remove_conversation("workspace-1", "removed")
            .map_err(|error| error.to_string())?;
        assert!(
            log.replay("workspace-1", Some("removed"), "turn", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );
        assert_eq!(
            log.replay("workspace-1", Some("retained"), "turn", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        log.remove_conversation("workspace-1", "removed")
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn conversation_stream_symlink_is_rejected_without_touching_target() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).map_err(|error| error.to_string())?;
        let marker = outside.join("keep.txt");
        fs::write(&marker, b"keep").map_err(|error| error.to_string())?;
        let stream_dir = log.stream_dir(TEST_TURN_1_STREAM_ID);
        symlink(&outside, &stream_dir).map_err(|error| error.to_string())?;

        assert!(matches!(
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            ),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        assert!(matches!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        assert!(matches!(
            log.remove_conversation("workspace-1", "conversation-1"),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        assert_eq!(
            fs::read(&marker).map_err(|error| error.to_string())?,
            b"keep"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn replaced_root_symlink_is_rejected_without_touching_target() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        fs::remove_dir(&root).map_err(|error| error.to_string())?;
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).map_err(|error| error.to_string())?;
        let marker = outside.join("keep.txt");
        fs::write(&marker, b"keep").map_err(|error| error.to_string())?;
        symlink(&outside, &root).map_err(|error| error.to_string())?;

        assert!(matches!(
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            ),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        assert!(matches!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        assert!(matches!(
            log.remove_conversation("workspace-1", "conversation-1"),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        assert_eq!(
            fs::read(marker).map_err(|error| error.to_string())?,
            b"keep"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn segment_symlink_replacement_is_rejected_without_touching_target() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path().join("events"), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::TurnStatus {
                status: "running".to_string(),
            },
        )
        .map_err(|error| error.to_string())?;
        let segment = list_segments(&log.stream_dir(TEST_TURN_1_STREAM_ID))
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .ok_or_else(|| "segment missing".to_string())?;
        let outside = temp.path().join("outside.jsonl");
        fs::write(&outside, b"outside\n").map_err(|error| error.to_string())?;
        fs::remove_file(&segment).map_err(|error| error.to_string())?;
        symlink(&outside, &segment).map_err(|error| error.to_string())?;

        assert!(
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .is_err()
        );
        assert_eq!(
            fs::read(&outside).map_err(|error| error.to_string())?,
            b"outside\n"
        );
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
            "input-other",
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

        let reopened = ChatEventLog::open(root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let workspace_a = reopened
            .queued_chat_inputs("workspace-a", "conversation-1")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            workspace_a
                .iter()
                .map(|input| input.input_id.as_str())
                .collect::<Vec<_>>(),
            vec!["input-a"]
        );
        let workspace_b = reopened
            .queued_chat_inputs("workspace-b", "conversation-1")
            .map_err(|error| error.to_string())?;
        assert_eq!(workspace_b.len(), 1);
        assert_eq!(
            workspace_b.first().map(|input| input.input_id.as_str()),
            Some("input-other")
        );
        Ok(())
    }

    #[test]
    fn workspace_removal_keeps_same_conversation_in_other_workspace() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path().join("events"), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for workspace_id in ["workspace-a", "workspace-b"] {
            log.append(
                workspace_id,
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        log.remove_workspace("workspace-a")
            .map_err(|error| error.to_string())?;
        assert!(
            log.replay("workspace-a", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );
        assert_eq!(
            log.replay("workspace-b", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        Ok(())
    }
}
