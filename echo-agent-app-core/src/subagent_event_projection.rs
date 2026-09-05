//! EKO projection of framework-owned Subagent execution envelopes.
//!
//! The framework owns attempt identity, ordering, replay, and terminal
//! reconciliation. This module adds only EKO workspace addressing, commits the
//! resulting [`ExecEvent`] to the existing [`ChatEventLog`], and derives tool
//! detail through the existing [`ToolExecutionProjector`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use echo_agent::agent::{StreamId, validate_envelope_trajectory};
use echo_agent::subagent::{
    SubagentEvent, SubagentEventBus, SubagentEventEnvelope, SubagentEventGap, SubagentEventReplay,
    SubagentStatus,
};

use crate::chat_driver::ChatDriverEvent;
use crate::chat_event_log::{ChatEventEnvelope, ChatEventLog, ChatEventLogError};
use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};
use crate::tasks::task_runtime::executor::{ExecEvent, SubagentEventMetadata};
use crate::tasks::task_runtime::{RuntimeEventKind, TaskRuntimeStore};
use crate::tool_execution::ToolExecutionRepository;
use crate::tool_execution_projection::{ToolExecutionProjectionUpdate, ToolExecutionProjector};
use crate::workspace::runtime::WorkspaceRuntimeRegistry;

const MAX_TRACKED_STREAMS: usize = 1024;
const MAX_PENDING_TOOL_PROJECTIONS: usize = 1024;
const MAX_COMMITTED_PROJECTION_REPLAY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionAddress {
    workspace_id: String,
    conversation_id: String,
    root_turn_id: String,
    run_id: String,
    execution_id: String,
}

#[derive(Debug, Default)]
struct ProjectionState {
    cursors: HashMap<String, u64>,
    addresses: HashMap<String, ProjectionAddress>,
    stream_order: VecDeque<String>,
}

impl ProjectionState {
    fn remember(&mut self, stream_id: &str, sequence: u64, address: ProjectionAddress) {
        self.cursors.insert(stream_id.to_string(), sequence);
        self.addresses.insert(stream_id.to_string(), address);
        if !self
            .stream_order
            .iter()
            .any(|candidate| candidate == stream_id)
        {
            self.stream_order.push_back(stream_id.to_string());
        }
        while self.stream_order.len() > MAX_TRACKED_STREAMS {
            if let Some(retired) = self.stream_order.pop_front() {
                self.cursors.remove(&retired);
                self.addresses.remove(&retired);
            }
        }
    }
}

/// One committed product event and its existing secondary tool projection.
#[derive(Debug, Clone)]
pub struct ProjectedSubagentEvent {
    pub envelope: ChatEventEnvelope,
    pub tool_updates: Vec<ToolExecutionProjectionUpdate>,
}

/// Retryable secondary projection backed by the already-committed ChatEventLog.
/// A failed detail write never causes the canonical execution event to be
/// appended twice; later events retry the exact retained envelope in order.
struct RecoverableToolExecutionProjection {
    inner: ToolExecutionProjector,
    pending: Mutex<VecDeque<ChatEventEnvelope>>,
}

impl RecoverableToolExecutionProjection {
    fn new(inner: ToolExecutionProjector) -> Self {
        Self {
            inner,
            pending: Mutex::new(VecDeque::new()),
        }
    }

    fn project_envelope(&self, envelope: &ChatEventEnvelope) -> Vec<ToolExecutionProjectionUpdate> {
        // Serialize debt so concurrent native projectors cannot overwrite each
        // other. Only repay the same live-sink route: updates from another
        // conversation must never ride this envelope.
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let existing = std::mem::take(&mut *pending);
        let mut candidates = VecDeque::new();
        let mut retained = VecDeque::new();
        for candidate in existing {
            if same_chat_route(&candidate, envelope) {
                candidates.push_back(candidate);
            } else {
                retained.push_back(candidate);
            }
        }
        if !candidates
            .iter()
            .any(|candidate| candidate.event_id == envelope.event_id)
        {
            candidates.push_back(envelope.clone());
        }

        let mut updates = Vec::new();
        let mut failed = VecDeque::new();
        for candidate in candidates {
            let mut projection = self.inner.project_envelope(&candidate);
            if projection.is_err() {
                // One immediate retry handles transient filesystem contention
                // even when this is the last event in an attempt.
                projection = self.inner.project_envelope(&candidate);
            }
            match projection {
                Ok(mut candidate_updates) => updates.append(&mut candidate_updates),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        event_id = %candidate.event_id,
                        "derived tool projection remains pending in the durable Chat event replay"
                    );
                    failed.push_back(candidate);
                }
            }
        }

        retained.extend(failed);
        while retained.len() > MAX_PENDING_TOOL_PROJECTIONS {
            if let Some(evicted) = retained.pop_front() {
                tracing::error!(
                    event_id = %evicted.event_id,
                    "in-memory tool projection retry window advanced; boot replay remains authoritative"
                );
            }
        }
        *pending = retained;
        updates
    }

    #[cfg(test)]
    fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

fn same_chat_route(left: &ChatEventEnvelope, right: &ChatEventEnvelope) -> bool {
    left.workspace_id == right.workspace_id
        && left.conversation_id == right.conversation_id
        && left.root_turn_id == right.root_turn_id
}

/// App-core boundary for EKO-native TaskRuntime execution events that do not
/// originate from a framework Subagent envelope (for example run-level and
/// primary-direct task events).
pub struct JournaledExecutionProjector {
    chat_events: Arc<ChatEventLog>,
    task_runtime: Arc<TaskRuntimeStore>,
    tool_projector: RecoverableToolExecutionProjection,
}

impl JournaledExecutionProjector {
    pub fn new(
        chat_events: Arc<ChatEventLog>,
        tool_executions: Arc<ToolExecutionRepository>,
        task_runtime: Arc<TaskRuntimeStore>,
    ) -> Self {
        Self {
            chat_events,
            task_runtime: task_runtime.clone(),
            tool_projector: RecoverableToolExecutionProjection::new(ToolExecutionProjector::new(
                tool_executions,
                Some(task_runtime),
            )),
        }
    }

    pub fn project(
        &self,
        event: ExecEvent,
    ) -> Result<ProjectedSubagentEvent, SubagentEventProjectionError> {
        let run = self
            .task_runtime
            .get_run(&event.run_id)
            .map_err(|error| SubagentEventProjectionError::TaskRuntime(error.to_string()))?
            .ok_or_else(|| {
                SubagentEventProjectionError::AddressUnavailable(format!(
                    "TaskRun '{}' was not found",
                    event.run_id
                ))
            })?;
        if run.workspace_id != event.workspace_id || run.conversation_id != event.conversation_id {
            return Err(SubagentEventProjectionError::InvalidIdentity(
                "TaskRuntime execution event address conflicts with its run".to_string(),
            ));
        }
        let envelope = self.chat_events.append(
            &run.workspace_id,
            Some(&run.conversation_id),
            &run.root_message_id,
            ChatDriverEvent::Execution(Box::new(event)),
        )?;
        let tool_updates = self.tool_projector.project_envelope(&envelope);
        Ok(ProjectedSubagentEvent {
            envelope,
            tool_updates,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubagentEventProjectionError {
    #[error("framework Subagent envelope is invalid: {0}")]
    InvalidEnvelope(String),
    #[error("framework Subagent identity is invalid: {0}")]
    InvalidIdentity(String),
    #[error("EKO Subagent projection address is unavailable: {0}")]
    AddressUnavailable(String),
    #[error("TaskRun projection lookup failed: {0}")]
    TaskRuntime(String),
    #[error(transparent)]
    Journal(#[from] ChatEventLogError),
}

/// Pure app-core adapter over the framework bus and EKO's existing stores.
pub struct SubagentEnvelopeProjector {
    bus: SubagentEventBus,
    global_task_runtime: Option<Arc<TaskRuntimeStore>>,
    workspace_runtimes: Arc<WorkspaceRuntimeRegistry>,
    foreground_turns: ForegroundTurnControl,
    chat_events: Arc<ChatEventLog>,
    tool_projector: RecoverableToolExecutionProjection,
    state: Mutex<ProjectionState>,
}

impl SubagentEnvelopeProjector {
    pub(crate) fn new(
        bus: SubagentEventBus,
        global_task_runtime: Option<Arc<TaskRuntimeStore>>,
        workspace_runtimes: Arc<WorkspaceRuntimeRegistry>,
        foreground_turns: ForegroundTurnControl,
        chat_events: Arc<ChatEventLog>,
        tool_executions: Arc<ToolExecutionRepository>,
    ) -> Self {
        Self {
            tool_projector: RecoverableToolExecutionProjection::new(ToolExecutionProjector::new(
                tool_executions,
                global_task_runtime.clone(),
            )),
            bus,
            global_task_runtime,
            workspace_runtimes,
            foreground_turns,
            chat_events,
            state: Mutex::new(ProjectionState::default()),
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Arc<SubagentEventEnvelope>> {
        self.bus.subscribe_envelopes()
    }

    /// Ingest one live envelope. A sequence jump is recovered from the
    /// framework's retained suffix before any newer event is committed.
    pub async fn ingest(
        &self,
        envelope: Arc<SubagentEventEnvelope>,
    ) -> Result<Vec<ProjectedSubagentEvent>, SubagentEventProjectionError> {
        validate_single_envelope(envelope.as_ref())?;
        let stream_id = envelope.stream_id.as_str().to_string();
        let last_sequence = self.cursor(&stream_id);
        if envelope.sequence <= last_sequence {
            return Ok(Vec::new());
        }
        if envelope.sequence == last_sequence.saturating_add(1) {
            return self
                .project_one(envelope.as_ref())
                .await
                .map(|event| vec![event]);
        }

        let replay = self.bus.replay_after(&envelope.stream_id, last_sequence);
        self.ingest_replay(Some(envelope), replay).await
    }

    /// Reconcile every retained framework stream after a broadcast lag,
    /// including short attempts EKO had not observed before the lag signal.
    pub async fn recover_known(
        &self,
    ) -> Result<Vec<ProjectedSubagentEvent>, SubagentEventProjectionError> {
        let known_cursors = {
            let state = self.lock_state();
            state
                .cursors
                .iter()
                .map(|(stream_id, sequence)| (stream_id.clone(), *sequence))
                .collect::<HashMap<_, _>>()
        };
        let mut stream_ids = self.bus.retained_stream_ids();
        stream_ids.extend(self.bus.active_stream_ids());
        for stream_id in known_cursors.keys() {
            stream_ids.push(StreamId::new(stream_id.clone()).map_err(|error| {
                SubagentEventProjectionError::InvalidEnvelope(error.to_string())
            })?);
        }
        stream_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        stream_ids.dedup();

        let mut projected = Vec::new();
        for stream_id in stream_ids {
            let sequence = known_cursors.get(stream_id.as_str()).copied().unwrap_or(0);
            projected.extend(
                self.ingest_replay(None, self.bus.replay_after(&stream_id, sequence))
                    .await?,
            );
        }
        Ok(projected)
    }

    async fn ingest_replay(
        &self,
        live: Option<Arc<SubagentEventEnvelope>>,
        replay: SubagentEventReplay,
    ) -> Result<Vec<ProjectedSubagentEvent>, SubagentEventProjectionError> {
        let mut candidates = replay.events;
        if let Some(terminal) = replay.terminal {
            candidates.push(terminal);
        }
        if let Some(live) = live {
            candidates.push(live);
        }
        if candidates.is_empty()
            && let Some(gap) = replay.gap.as_ref()
            && let Some(anchor) = self.bus.active_stream_anchor(&gap.stream_id)
        {
            candidates.push(anchor);
        }
        candidates.sort_by_key(|event| event.sequence);
        let mut event_ids = HashSet::new();
        candidates.retain(|event| event_ids.insert(event.event_id.as_str().to_string()));

        let mut projected = Vec::new();
        if let Some(gap) = replay.gap {
            let before_gap = candidates
                .iter()
                .take_while(|event| event.sequence <= gap.requested_after)
                .cloned()
                .collect::<Vec<_>>();
            projected.extend(self.project_candidates(before_gap).await?);
            let exemplar = candidates.first().ok_or_else(|| {
                SubagentEventProjectionError::AddressUnavailable(format!(
                    "stream '{}' reported a gap without a retained event",
                    gap.stream_id
                ))
            })?;
            projected.push(self.project_gap(&gap, exemplar.as_ref()).await?);
            let skipped_through = gap
                .available_from
                .map(|sequence| sequence.saturating_sub(1))
                .unwrap_or(gap.latest_sequence);
            self.set_cursor(gap.stream_id.as_str(), skipped_through);
        }
        projected.extend(self.project_candidates(candidates).await?);
        Ok(projected)
    }

    async fn project_candidates(
        &self,
        candidates: Vec<Arc<SubagentEventEnvelope>>,
    ) -> Result<Vec<ProjectedSubagentEvent>, SubagentEventProjectionError> {
        let mut projected = Vec::new();
        for event in candidates {
            let last_sequence = self.cursor(event.stream_id.as_str());
            if event.sequence <= last_sequence {
                continue;
            }
            if event.sequence != last_sequence.saturating_add(1) {
                return Err(SubagentEventProjectionError::InvalidEnvelope(format!(
                    "stream '{}' remains non-contiguous after replay: expected {}, got {}",
                    event.stream_id,
                    last_sequence.saturating_add(1),
                    event.sequence
                )));
            }
            projected.push(self.project_one(event.as_ref()).await?);
        }
        Ok(projected)
    }

    async fn project_gap(
        &self,
        gap: &SubagentEventGap,
        exemplar: &SubagentEventEnvelope,
    ) -> Result<ProjectedSubagentEvent, SubagentEventProjectionError> {
        let address = self.resolve_address(exemplar).await?;
        let execution = ExecEvent::subagent_attempt(
            address.workspace_id.clone(),
            address.conversation_id.clone(),
            address.run_id.clone(),
            exemplar.payload.invocation.task_id.clone(),
            address.execution_id.clone(),
            RuntimeEventKind::SubagentStreamGap,
            serde_json::json!({
                "stream_id": gap.stream_id.as_str(),
                "requested_after": gap.requested_after,
                "available_from": gap.available_from,
                "latest_sequence": gap.latest_sequence,
            }),
        )
        .with_agent(exemplar.payload.invocation.agent_name.clone());
        self.commit(address, execution)
    }

    async fn project_one(
        &self,
        envelope: &SubagentEventEnvelope,
    ) -> Result<ProjectedSubagentEvent, SubagentEventProjectionError> {
        validate_single_envelope(envelope)?;
        validate_route_identity(envelope)?;
        let address = self.resolve_address(envelope).await?;
        let event = runtime_event_kind(&envelope.payload.event);
        let mut payload = event_payload(&envelope.payload.event)?;
        if event == RuntimeEventKind::Usage
            && let serde_json::Value::Object(fields) = &mut payload
        {
            fields.insert(
                "usage_event_id".to_string(),
                envelope.event_id.as_str().into(),
            );
        }
        let metadata = SubagentEventMetadata {
            schema_version: envelope.schema_version,
            event_id: envelope.event_id.as_str().to_string(),
            content_hash: envelope.content_hash.clone(),
            sequence: envelope.sequence,
            stream_id: envelope.stream_id.as_str().to_string(),
            turn_id: envelope.turn_id.as_str().to_string(),
            message_id: envelope
                .message_id
                .as_ref()
                .map(|value| value.as_str().to_string()),
            execution_id: address.execution_id.clone(),
            parent_event_id: envelope
                .parent_event_id
                .as_ref()
                .map(|value| value.as_str().to_string()),
            timestamp: envelope.timestamp.to_rfc3339(),
            parent_agent: envelope.payload.invocation.parent_agent.clone(),
            agent_name: envelope.payload.invocation.agent_name.clone(),
            parent_execution_id: envelope.payload.invocation.parent_execution_id.clone(),
            agent_path: envelope.payload.invocation.agent_path.clone(),
            task_id: envelope.payload.invocation.task_id.clone(),
            attempt: envelope.payload.invocation.attempt,
            plan_revision: envelope.payload.invocation.plan_revision,
        };
        let execution = ExecEvent::subagent_attempt(
            address.workspace_id.clone(),
            address.conversation_id.clone(),
            address.run_id.clone(),
            envelope.payload.invocation.task_id.clone(),
            address.execution_id.clone(),
            event,
            payload,
        )
        .with_agent(envelope.payload.invocation.agent_name.clone())
        .with_framework_event(metadata);
        let committed = self.commit(address.clone(), execution)?;
        self.lock_state()
            .remember(envelope.stream_id.as_str(), envelope.sequence, address);
        Ok(committed)
    }

    fn commit(
        &self,
        address: ProjectionAddress,
        event: ExecEvent,
    ) -> Result<ProjectedSubagentEvent, SubagentEventProjectionError> {
        let envelope = self.chat_events.append(
            &address.workspace_id,
            Some(&address.conversation_id),
            &address.root_turn_id,
            ChatDriverEvent::Execution(Box::new(event)),
        )?;
        let tool_updates = self.tool_projector.project_envelope(&envelope);
        Ok(ProjectedSubagentEvent {
            envelope,
            tool_updates,
        })
    }

    async fn resolve_address(
        &self,
        envelope: &SubagentEventEnvelope,
    ) -> Result<ProjectionAddress, SubagentEventProjectionError> {
        if let Some(address) = self
            .lock_state()
            .addresses
            .get(envelope.stream_id.as_str())
            .cloned()
        {
            validate_cached_address(envelope, &address)?;
            return Ok(address);
        }
        if let Some(run_id) = envelope.run_id.as_ref() {
            return self
                .resolve_task_run_address(envelope, run_id.as_str())
                .await;
        }
        self.resolve_foreground_address(envelope)
    }

    async fn resolve_task_run_address(
        &self,
        envelope: &SubagentEventEnvelope,
        run_id: &str,
    ) -> Result<ProjectionAddress, SubagentEventProjectionError> {
        let (task_runtime, run) = self
            .workspace_runtimes
            .resolve_run_owner(self.global_task_runtime.clone(), run_id)
            .await
            .map_err(|error| SubagentEventProjectionError::TaskRuntime(error.to_string()))?
            .ok_or_else(|| {
                SubagentEventProjectionError::AddressUnavailable(format!(
                    "TaskRun '{run_id}' was not found"
                ))
            })?;
        if envelope
            .conversation_id
            .as_ref()
            .is_some_and(|value| value.as_str() != run.conversation_id)
        {
            return Err(SubagentEventProjectionError::InvalidIdentity(
                "framework conversation does not match TaskRun".to_string(),
            ));
        }
        if envelope
            .message_id
            .as_ref()
            .is_some_and(|value| value.as_str() != run.root_message_id)
        {
            return Err(SubagentEventProjectionError::InvalidIdentity(
                "framework message does not match TaskRun root message".to_string(),
            ));
        }
        let execution_id = required_execution_id(envelope)?;
        if let Some(task_id) = envelope.payload.invocation.task_id.as_deref() {
            let attempt = envelope.payload.invocation.attempt.ok_or_else(|| {
                SubagentEventProjectionError::InvalidIdentity(
                    "TaskRun Subagent envelope lacks an attempt".to_string(),
                )
            })?;
            let plan_revision = envelope.payload.invocation.plan_revision.ok_or_else(|| {
                SubagentEventProjectionError::InvalidIdentity(
                    "TaskRun Subagent envelope lacks a plan revision".to_string(),
                )
            })?;
            let lookup_run_id = run_id.to_string();
            let lookup_execution_id = execution_id.clone();
            let snapshot = tokio::task::spawn_blocking(move || {
                task_runtime.get_subagent_run_snapshot(&lookup_run_id, &lookup_execution_id)
            })
            .await
            .map_err(|error| {
                SubagentEventProjectionError::TaskRuntime(format!(
                    "Subagent identity lookup task failed: {error}"
                ))
            })?
            .map_err(|error| SubagentEventProjectionError::TaskRuntime(error.to_string()))?
            .ok_or_else(|| {
                SubagentEventProjectionError::InvalidIdentity(format!(
                    "TaskRun '{run_id}' has no assigned Subagent execution '{execution_id}'"
                ))
            })?;
            if snapshot.run.task_id != task_id
                || snapshot.run.subagent_name != envelope.payload.invocation.agent_name
                || snapshot.run.attempt != attempt
                || snapshot.plan_revision != Some(plan_revision)
            {
                return Err(SubagentEventProjectionError::InvalidIdentity(
                    "framework Subagent attempt conflicts with TaskRuntime assignment".to_string(),
                ));
            }
        }
        Ok(ProjectionAddress {
            workspace_id: run.workspace_id,
            conversation_id: run.conversation_id,
            root_turn_id: run.root_message_id,
            run_id: run_id.to_string(),
            execution_id,
        })
    }

    fn resolve_foreground_address(
        &self,
        envelope: &SubagentEventEnvelope,
    ) -> Result<ProjectionAddress, SubagentEventProjectionError> {
        let conversation_id = envelope
            .conversation_id
            .as_ref()
            .map(|value| value.as_str())
            .ok_or_else(|| {
                SubagentEventProjectionError::AddressUnavailable(
                    "ordinary Subagent envelope lacks conversation id".to_string(),
                )
            })?;
        let message_id = envelope
            .message_id
            .as_ref()
            .map(|value| value.as_str())
            .unwrap_or_else(|| envelope.turn_id.as_str());
        let mut matches = Vec::new();
        for surface in [
            ForegroundTurnSurface::Gui,
            ForegroundTurnSurface::Tui,
            ForegroundTurnSurface::Cli,
            ForegroundTurnSurface::Channel,
            ForegroundTurnSurface::Agent,
        ] {
            let snapshots = self.foreground_turns.snapshots(surface).map_err(|error| {
                SubagentEventProjectionError::AddressUnavailable(error.to_string())
            })?;
            matches.extend(snapshots.into_iter().filter(|snapshot| {
                snapshot.conversation_id == conversation_id
                    && (snapshot.root_turn_id == message_id
                        || snapshot.active_turn_id == envelope.turn_id.as_str())
            }));
        }
        let first = matches.first().cloned().ok_or_else(|| {
            SubagentEventProjectionError::AddressUnavailable(format!(
                "no active foreground turn matches conversation '{conversation_id}' and message '{message_id}'"
            ))
        })?;
        if matches.iter().skip(1).any(|candidate| {
            candidate.workspace_id != first.workspace_id
                || candidate.conversation_id != first.conversation_id
                || candidate.root_turn_id != first.root_turn_id
        }) {
            return Err(SubagentEventProjectionError::AddressUnavailable(format!(
                "multiple foreground turns match conversation '{conversation_id}' and message '{message_id}'"
            )));
        }
        Ok(ProjectionAddress {
            workspace_id: first.workspace_id,
            conversation_id: first.conversation_id,
            root_turn_id: first.root_turn_id,
            run_id: String::new(),
            execution_id: required_execution_id(envelope)?,
        })
    }

    fn cursor(&self, stream_id: &str) -> u64 {
        self.lock_state()
            .cursors
            .get(stream_id)
            .copied()
            .unwrap_or(0)
    }

    fn set_cursor(&self, stream_id: &str, sequence: u64) {
        self.lock_state()
            .cursors
            .insert(stream_id.to_string(), sequence);
    }

    fn lock_state(&self) -> MutexGuard<'_, ProjectionState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Subagent projection state lock was poisoned; recovering state");
            poisoned.into_inner()
        })
    }
}

/// One process-owned consumer for the shared Subagent event bus.
pub struct SubagentEnvelopeProjectionService {
    cancel: tokio_util::sync::CancellationToken,
    handle: tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), String>>>>,
    committed: tokio::sync::broadcast::Sender<Arc<ProjectedSubagentEvent>>,
    committed_replay: Arc<Mutex<VecDeque<Arc<ProjectedSubagentEvent>>>>,
}

impl SubagentEnvelopeProjectionService {
    pub(crate) fn start(projector: Arc<SubagentEnvelopeProjector>) -> Arc<Self> {
        let cancel = tokio_util::sync::CancellationToken::new();
        let (committed, _) = tokio::sync::broadcast::channel(256);
        let committed_replay = Arc::new(Mutex::new(VecDeque::new()));
        let mut receiver = projector.subscribe();
        let task_cancel = cancel.clone();
        let task_projector = projector.clone();
        let task_committed = committed.clone();
        let task_committed_replay = Arc::clone(&committed_replay);
        let handle = tokio::spawn(async move {
            loop {
                let received = tokio::select! {
                    _ = task_cancel.cancelled() => {
                        while let Ok(envelope) = receiver.try_recv() {
                            deliver_projection_result(
                                &task_projector,
                                &task_committed,
                                &task_committed_replay,
                                task_projector.ingest(envelope).await,
                            );
                        }
                        deliver_projection_result(
                            &task_projector,
                            &task_committed,
                            &task_committed_replay,
                            task_projector.recover_known().await,
                        );
                        break;
                    },
                    received = receiver.recv() => received,
                };
                let events = match received {
                    Ok(envelope) => task_projector.ingest(envelope).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            skipped,
                            "Subagent envelope receiver lagged; reconciling retained streams"
                        );
                        task_projector.recover_known().await
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                deliver_projection_result(
                    &task_projector,
                    &task_committed,
                    &task_committed_replay,
                    events,
                );
            }
            Ok(())
        });
        Arc::new(Self {
            cancel,
            handle: tokio::sync::Mutex::new(Some(handle)),
            committed,
            committed_replay,
        })
    }

    /// Subscribe to committed events that had no live turn sink. Surface
    /// bridges use this for background completions without reading framework
    /// execution events directly.
    pub fn subscribe_committed(
        &self,
    ) -> tokio::sync::broadcast::Receiver<Arc<ProjectedSubagentEvent>> {
        self.committed.subscribe()
    }

    /// Snapshot the bounded process-level fallback window. Callers subscribe
    /// first, then apply this snapshot and deduplicate by Chat event id so an
    /// event racing with subscription is neither lost nor rendered twice.
    pub fn replay_committed(&self) -> Vec<Arc<ProjectedSubagentEvent>> {
        self.committed_replay
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    pub async fn shutdown_and_join(&self) -> Result<(), String> {
        self.cancel.cancel();
        let handle = self.handle.lock().await.take();
        match handle {
            Some(handle) => handle
                .await
                .map_err(|error| format!("Subagent projection task join failed: {error}"))?,
            None => Ok(()),
        }
    }

    pub fn abort(&self) {
        self.cancel.cancel();
        if let Ok(mut handle) = self.handle.try_lock()
            && let Some(handle) = handle.take()
        {
            handle.abort();
        }
    }
}

fn deliver_projection_result(
    projector: &SubagentEnvelopeProjector,
    committed: &tokio::sync::broadcast::Sender<Arc<ProjectedSubagentEvent>>,
    committed_replay: &Mutex<VecDeque<Arc<ProjectedSubagentEvent>>>,
    result: Result<Vec<ProjectedSubagentEvent>, SubagentEventProjectionError>,
) {
    match result {
        Ok(events) => {
            for event in events {
                let delivery = projector
                    .chat_events
                    .deliver_projected_event_with_status(&event.envelope, &event.tool_updates);
                if !delivery.had_live_sink || !delivery.all_succeeded {
                    let event = Arc::new(event);
                    {
                        let mut replay = committed_replay
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        replay.push_back(Arc::clone(&event));
                        while replay.len() > MAX_COMMITTED_PROJECTION_REPLAY {
                            if let Some(evicted) = replay.pop_front() {
                                tracing::warn!(
                                    event_id = %evicted.envelope.event_id,
                                    "committed Subagent fallback replay window advanced; ChatEventLog remains authoritative"
                                );
                            }
                        }
                    }
                    let _ = committed.send(event);
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, "failed to project framework Subagent event");
        }
    }
}

fn validate_single_envelope(
    envelope: &SubagentEventEnvelope,
) -> Result<(), SubagentEventProjectionError> {
    if envelope.sequence == 0 || envelope.payload.invocation.attempt == Some(0) {
        return Err(SubagentEventProjectionError::InvalidEnvelope(
            "Subagent event sequence and attempt must be one-based".to_string(),
        ));
    }
    let violations = validate_envelope_trajectory(std::slice::from_ref(envelope));
    if violations.is_empty() {
        Ok(())
    } else {
        Err(SubagentEventProjectionError::InvalidEnvelope(
            violations.join("; "),
        ))
    }
}

fn required_execution_id(
    envelope: &SubagentEventEnvelope,
) -> Result<String, SubagentEventProjectionError> {
    envelope
        .execution_id
        .as_ref()
        .map(|value| value.as_str().to_string())
        .ok_or_else(|| {
            SubagentEventProjectionError::InvalidIdentity(
                "Subagent envelope lacks an execution id".to_string(),
            )
        })
}

fn validate_cached_address(
    envelope: &SubagentEventEnvelope,
    address: &ProjectionAddress,
) -> Result<(), SubagentEventProjectionError> {
    if required_execution_id(envelope)? != address.execution_id
        || envelope
            .run_id
            .as_ref()
            .map(|value| value.as_str())
            .unwrap_or_default()
            != address.run_id
        || envelope
            .conversation_id
            .as_ref()
            .is_some_and(|value| value.as_str() != address.conversation_id)
    {
        return Err(SubagentEventProjectionError::InvalidIdentity(
            "framework stream identity changed after EKO address resolution".to_string(),
        ));
    }
    Ok(())
}

fn validate_route_identity(
    envelope: &SubagentEventEnvelope,
) -> Result<(), SubagentEventProjectionError> {
    let (parent, agent, execution_id, run_id) = event_route(&envelope.payload.event);
    if parent != envelope.payload.invocation.parent_agent
        || agent != envelope.payload.invocation.agent_name
        || execution_id != envelope.execution_id.as_ref().map(|value| value.as_str())
        || run_id != envelope.run_id.as_ref().map(|value| value.as_str())
    {
        return Err(SubagentEventProjectionError::InvalidIdentity(
            "Subagent payload route conflicts with envelope identity".to_string(),
        ));
    }
    if let SubagentEvent::DispatchStarted {
        conversation_id,
        message_id,
        ..
    } = &envelope.payload.event
        && (conversation_id.as_deref()
            != envelope
                .conversation_id
                .as_ref()
                .map(|value| value.as_str())
            || message_id.as_deref() != envelope.message_id.as_ref().map(|value| value.as_str()))
    {
        return Err(SubagentEventProjectionError::InvalidIdentity(
            "Subagent start address conflicts with envelope identity".to_string(),
        ));
    }
    Ok(())
}

fn event_route(event: &SubagentEvent) -> (&str, &str, Option<&str>, Option<&str>) {
    match event {
        SubagentEvent::UplinkReceived {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchStarted {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchIsolationObserved {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchCompleted {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchFailed {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchCancelled {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchThinkingStarted {
            parent,
            agent,
            execution_id,
            run_id,
        }
        | SubagentEvent::DispatchThinkingDelta {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchThinkingEnded {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchTokenDelta {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchLlmUsage {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchToolStarted {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        }
        | SubagentEvent::DispatchToolCompleted {
            parent,
            agent,
            execution_id,
            run_id,
            ..
        } => (parent, agent, execution_id.as_deref(), run_id.as_deref()),
        SubagentEvent::Registered { name } | SubagentEvent::Unregistered { name } => {
            ("", name, None, None)
        }
    }
}

fn runtime_event_kind(event: &SubagentEvent) -> RuntimeEventKind {
    match event {
        SubagentEvent::UplinkReceived { .. } => RuntimeEventKind::SubagentEscalationRequested,
        SubagentEvent::DispatchStarted { .. } => RuntimeEventKind::Started,
        SubagentEvent::DispatchIsolationObserved { .. } => RuntimeEventKind::IsolationObserved,
        SubagentEvent::DispatchCompleted { .. } => RuntimeEventKind::Completed,
        SubagentEvent::DispatchFailed { status, .. } => match status {
            SubagentStatus::TimedOut => RuntimeEventKind::TimedOut,
            SubagentStatus::Cancelled => RuntimeEventKind::Cancelled,
            SubagentStatus::Running | SubagentStatus::Completed | SubagentStatus::Failed => {
                RuntimeEventKind::Failed
            }
        },
        SubagentEvent::DispatchCancelled { .. } => RuntimeEventKind::Cancelled,
        SubagentEvent::DispatchThinkingStarted { .. } => RuntimeEventKind::ThinkingStarted,
        SubagentEvent::DispatchThinkingDelta { .. } => RuntimeEventKind::ThinkingDelta,
        SubagentEvent::DispatchThinkingEnded { .. } => RuntimeEventKind::ThinkingEnded,
        SubagentEvent::DispatchTokenDelta { .. } => RuntimeEventKind::TokenDelta,
        SubagentEvent::DispatchLlmUsage { .. } => RuntimeEventKind::Usage,
        SubagentEvent::DispatchToolStarted { .. } => RuntimeEventKind::ToolStarted,
        SubagentEvent::DispatchToolCompleted { .. } => RuntimeEventKind::ToolCompleted,
        SubagentEvent::Registered { .. } | SubagentEvent::Unregistered { .. } => {
            RuntimeEventKind::Note
        }
    }
}

fn event_payload(event: &SubagentEvent) -> Result<serde_json::Value, SubagentEventProjectionError> {
    match event {
        SubagentEvent::DispatchToolStarted {
            call_id,
            invocation,
            ..
        } => {
            return Ok(serde_json::json!({
                "call_id": call_id,
                "invocation": invocation,
            }));
        }
        SubagentEvent::DispatchToolCompleted {
            call_id,
            name,
            result,
            ..
        } => {
            return Ok(serde_json::json!({
                "call_id": call_id,
                "name": name,
                "result": result,
            }));
        }
        _ => {}
    }
    let value = serde_json::to_value(event)
        .map_err(|error| SubagentEventProjectionError::InvalidEnvelope(error.to_string()))?;
    let mut variants = value.as_object().into_iter().flatten();
    let (_, payload) = variants.next().ok_or_else(|| {
        SubagentEventProjectionError::InvalidEnvelope(
            "Subagent event did not serialize as one variant".to_string(),
        )
    })?;
    if variants.next().is_some() {
        return Err(SubagentEventProjectionError::InvalidEnvelope(
            "Subagent event serialized with multiple variants".to_string(),
        ));
    }
    Ok(payload.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_execution_projection::ToolExecutionProjectionKind;
    use echo_agent::agent::{EventEnvelope, EventIdentity};
    use echo_agent::subagent::{
        ExecutionMode, ObservedIsolation, SubagentEventPayload, SubagentInvocationIdentity,
        SubagentOutcome,
    };
    use echo_agent::tools::ToolResult;
    use tempfile::TempDir;

    fn ordinary_envelope(
        sequence: u64,
        event: SubagentEvent,
    ) -> Result<SubagentEventEnvelope, String> {
        let identity = EventIdentity::new("subagent-stream", "message-1")
            .map_err(|error| error.to_string())?
            .with_conversation_id("conversation-1")
            .map_err(|error| error.to_string())?
            .with_message_id("message-1")
            .map_err(|error| error.to_string())?
            .with_execution_id("execution-1")
            .map_err(|error| error.to_string())?;
        EventEnvelope::new(
            &identity,
            sequence,
            None,
            SubagentEventPayload {
                invocation: SubagentInvocationIdentity {
                    parent_agent: "primary".to_string(),
                    agent_name: "explorer".to_string(),
                    parent_execution_id: None,
                    agent_path: Some("primary/explorer".to_string()),
                    task_id: None,
                    attempt: Some(1),
                    plan_revision: None,
                },
                event,
            },
        )
        .map_err(|error| error.to_string())
    }

    fn test_projector(
        bus: SubagentEventBus,
        foreground_turns: ForegroundTurnControl,
        temp: &TempDir,
    ) -> Result<(Arc<SubagentEnvelopeProjector>, Arc<ChatEventLog>), String> {
        let chat_events = Arc::new(
            ChatEventLog::open(
                temp.path().join("chat-events"),
                crate::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        let tools = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tool-executions"))
                .map_err(|error| error.to_string())?,
        );
        let projector = Arc::new(SubagentEnvelopeProjector::new(
            bus,
            None,
            Arc::new(WorkspaceRuntimeRegistry::new()),
            foreground_turns,
            chat_events.clone(),
            tools,
        ));
        Ok((projector, chat_events))
    }

    #[test]
    fn maps_every_display_event_without_parsing_execution_identity() -> Result<(), String> {
        let started = ordinary_envelope(
            1,
            SubagentEvent::DispatchStarted {
                parent: "primary".to_string(),
                agent: "explorer".to_string(),
                mode: ExecutionMode::Sync,
                task: "inspect".to_string(),
                execution_id: Some("execution-1".to_string()),
                run_id: None,
                conversation_id: Some("conversation-1".to_string()),
                message_id: Some("message-1".to_string()),
                background: false,
            },
        )?;
        validate_route_identity(&started).map_err(|error| error.to_string())?;
        assert_eq!(
            runtime_event_kind(&started.payload.event),
            RuntimeEventKind::Started
        );
        let payload = event_payload(&started.payload.event).map_err(|error| error.to_string())?;
        assert_eq!(
            payload.get("task").and_then(serde_json::Value::as_str),
            Some("inspect")
        );
        let outcome = SubagentOutcome::terminal(SubagentStatus::Completed, "done", Vec::new());
        let classes = vec![
            (
                SubagentEvent::UplinkReceived {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    direction: "parent".to_string(),
                    status: "event_emitted".to_string(),
                    summary: "question".to_string(),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::SubagentEscalationRequested,
            ),
            (started.payload.event, RuntimeEventKind::Started),
            (
                SubagentEvent::DispatchIsolationObserved {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    isolation: ObservedIsolation::new("context"),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::IsolationObserved,
            ),
            (
                SubagentEvent::DispatchThinkingStarted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::ThinkingStarted,
            ),
            (
                SubagentEvent::DispatchThinkingDelta {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    content: "think".to_string(),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::ThinkingDelta,
            ),
            (
                SubagentEvent::DispatchThinkingEnded {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    prompt_tokens: 2,
                    completion_tokens: 3,
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::ThinkingEnded,
            ),
            (
                SubagentEvent::DispatchTokenDelta {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    content: "answer".to_string(),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::TokenDelta,
            ),
            (
                SubagentEvent::DispatchLlmUsage {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    model: "test".to_string(),
                    prompt_tokens: 2,
                    completion_tokens: 3,
                    total_tokens: 5,
                    cached_prompt_tokens: 1,
                    cache_creation_prompt_tokens: 0,
                    usage_reported: true,
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::Usage,
            ),
            (
                SubagentEvent::DispatchToolStarted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    call_id: "call-1".to_string(),
                    invocation: echo_agent::agent::ToolInvocation {
                        requested_name: "read_file".to_string(),
                        requested_args: serde_json::json!({"path": "README.md"}),
                        name: "read_file".to_string(),
                        args: serde_json::json!({"path": "README.md"}),
                        rewrites: Vec::new(),
                    },
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::ToolStarted,
            ),
            (
                SubagentEvent::DispatchToolCompleted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    call_id: "call-1".to_string(),
                    name: "read_file".to_string(),
                    result: ToolResult::success("ok"),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::ToolCompleted,
            ),
            (
                SubagentEvent::DispatchCompleted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    duration_ms: 10,
                    tokens_used: Some(5),
                    iterations: Some(1),
                    output: "done".to_string(),
                    outcome: outcome.clone(),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::Completed,
            ),
            (
                SubagentEvent::DispatchFailed {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    error: "failed".to_string(),
                    status: SubagentStatus::TimedOut,
                    outcome: SubagentOutcome::terminal(
                        SubagentStatus::TimedOut,
                        "failed",
                        vec!["continue".to_string()],
                    ),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::TimedOut,
            ),
            (
                SubagentEvent::DispatchCancelled {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    outcome: SubagentOutcome::terminal(
                        SubagentStatus::Cancelled,
                        "cancelled",
                        Vec::new(),
                    ),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
                RuntimeEventKind::Cancelled,
            ),
        ];
        for (event, expected) in classes {
            assert_eq!(runtime_event_kind(&event), expected);
            assert!(
                event_payload(&event)
                    .map_err(|error| error.to_string())?
                    .is_object()
            );
        }
        Ok(())
    }

    #[test]
    fn rejects_payload_route_that_conflicts_with_envelope() -> Result<(), String> {
        let envelope = ordinary_envelope(
            1,
            SubagentEvent::DispatchThinkingStarted {
                parent: "primary".to_string(),
                agent: "reviewer".to_string(),
                execution_id: Some("execution-1".to_string()),
                run_id: None,
            },
        )?;
        assert!(matches!(
            validate_route_identity(&envelope),
            Err(SubagentEventProjectionError::InvalidIdentity(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn projects_ordered_events_once_into_existing_chat_log() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let foreground_turns = ForegroundTurnControl::default();
        let _turn = foreground_turns
            .begin_scoped(
                "workspace-1",
                ForegroundTurnSurface::Gui,
                "conversation-1",
                "message-1",
            )
            .map_err(|error| error.to_string())?;
        let (projector, chat_events) =
            test_projector(SubagentEventBus::new(), foreground_turns, &temp)?;
        let events = [
            ordinary_envelope(
                1,
                SubagentEvent::DispatchStarted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    mode: ExecutionMode::Sync,
                    task: "inspect".to_string(),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                    conversation_id: Some("conversation-1".to_string()),
                    message_id: Some("message-1".to_string()),
                    background: false,
                },
            )?,
            ordinary_envelope(
                2,
                SubagentEvent::DispatchThinkingDelta {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    content: "reasoning".to_string(),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
            )?,
            ordinary_envelope(
                3,
                SubagentEvent::DispatchTokenDelta {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    content: "answer".to_string(),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
            )?,
            ordinary_envelope(
                4,
                SubagentEvent::DispatchCompleted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    duration_ms: 12,
                    tokens_used: Some(8),
                    iterations: Some(1),
                    output: "answer".to_string(),
                    outcome: SubagentOutcome::terminal(
                        SubagentStatus::Completed,
                        "done",
                        Vec::new(),
                    ),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
            )?,
        ];
        for event in events {
            let projected = projector
                .ingest(Arc::new(event))
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(projected.len(), 1);
        }
        let duplicate = projector
            .ingest(Arc::new(ordinary_envelope(
                4,
                SubagentEvent::DispatchCompleted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    duration_ms: 12,
                    tokens_used: Some(8),
                    iterations: Some(1),
                    output: "answer".to_string(),
                    outcome: SubagentOutcome::terminal(
                        SubagentStatus::Completed,
                        "done",
                        Vec::new(),
                    ),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                },
            )?))
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.is_empty());

        let replay = chat_events
            .replay("workspace-1", Some("conversation-1"), "message-1", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.events.len(), 4);
        let sequences = replay
            .events
            .iter()
            .filter_map(|event| match &event.payload {
                ChatDriverEvent::Execution(event) => event
                    .framework_event
                    .as_ref()
                    .map(|metadata| metadata.sequence),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![1, 2, 3, 4]);
        Ok(())
    }

    #[tokio::test]
    async fn formal_event_resolves_its_task_runtime_owner_without_ui_focus() -> Result<(), String> {
        use crate::tasks::task_runtime::{AttendedMode, DomainProfile};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let task_runtime = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
                .map_err(|error| error.to_string())?,
        );
        task_runtime
            .create_run(
                "run-1",
                "workspace-task",
                "conversation-task",
                "message-task",
                DomainProfile::General,
                "inspect",
                "task_runtime",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        task_runtime
            .record_subagent_assigned(
                "run-1",
                "task-with:colons",
                "opaque-execution",
                "explorer",
                "Inspect",
                7,
                2,
                true,
                false,
            )
            .map_err(|error| error.to_string())?;
        let bus = SubagentEventBus::new();
        let chat_events = Arc::new(
            ChatEventLog::open(
                temp.path().join("chat-events"),
                crate::chat_event_log::ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?,
        );
        let projector = SubagentEnvelopeProjector::new(
            bus,
            Some(task_runtime),
            Arc::new(WorkspaceRuntimeRegistry::new()),
            ForegroundTurnControl::default(),
            chat_events.clone(),
            Arc::new(
                ToolExecutionRepository::open(temp.path().join("tools"))
                    .map_err(|error| error.to_string())?,
            ),
        );
        let identity = EventIdentity::new("formal-stream", "message-task")
            .map_err(|error| error.to_string())?
            .with_conversation_id("conversation-task")
            .map_err(|error| error.to_string())?
            .with_run_id("run-1")
            .map_err(|error| error.to_string())?
            .with_message_id("message-task")
            .map_err(|error| error.to_string())?
            .with_execution_id("opaque-execution")
            .map_err(|error| error.to_string())?;
        let envelope = EventEnvelope::new(
            &identity,
            1,
            None,
            SubagentEventPayload {
                invocation: SubagentInvocationIdentity {
                    parent_agent: "primary".to_string(),
                    agent_name: "explorer".to_string(),
                    parent_execution_id: None,
                    agent_path: Some("primary/explorer".to_string()),
                    task_id: Some("task-with:colons".to_string()),
                    attempt: Some(2),
                    plan_revision: Some(7),
                },
                event: SubagentEvent::DispatchStarted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    mode: ExecutionMode::Fork,
                    task: "inspect".to_string(),
                    execution_id: Some("opaque-execution".to_string()),
                    run_id: Some("run-1".to_string()),
                    conversation_id: Some("conversation-task".to_string()),
                    message_id: Some("message-task".to_string()),
                    background: false,
                },
            },
        )
        .map_err(|error| error.to_string())?;
        projector
            .ingest(Arc::new(envelope))
            .await
            .map_err(|error| error.to_string())?;

        let replay = chat_events
            .replay(
                "workspace-task",
                Some("conversation-task"),
                "message-task",
                0,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            replay.events.first().map(|event| &event.payload),
            Some(ChatDriverEvent::Execution(event))
                if event.task_id.as_deref() == Some("task-with:colons")
                    && event.subagent_run_id.as_deref() == Some("opaque-execution")
        ));

        let conflicting_identity = EventIdentity::new("formal-conflict", "message-task")
            .map_err(|error| error.to_string())?
            .with_conversation_id("conversation-task")
            .map_err(|error| error.to_string())?
            .with_run_id("run-1")
            .map_err(|error| error.to_string())?
            .with_message_id("message-task")
            .map_err(|error| error.to_string())?
            .with_execution_id("opaque-execution")
            .map_err(|error| error.to_string())?;
        let conflict = EventEnvelope::new(
            &conflicting_identity,
            1,
            None,
            SubagentEventPayload {
                invocation: SubagentInvocationIdentity {
                    parent_agent: "primary".to_string(),
                    agent_name: "explorer".to_string(),
                    parent_execution_id: None,
                    agent_path: Some("primary/explorer".to_string()),
                    task_id: Some("task-with:colons".to_string()),
                    attempt: Some(3),
                    plan_revision: Some(7),
                },
                event: SubagentEvent::DispatchThinkingStarted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    execution_id: Some("opaque-execution".to_string()),
                    run_id: Some("run-1".to_string()),
                },
            },
        )
        .map_err(|error| error.to_string())?;
        assert!(matches!(
            projector.ingest(Arc::new(conflict)).await,
            Err(SubagentEventProjectionError::InvalidIdentity(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn continuation_event_keeps_framework_active_turn_in_chat_envelope() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let foreground_turns = ForegroundTurnControl::default();
        let _turn = foreground_turns
            .begin_scoped(
                "workspace-1",
                ForegroundTurnSurface::Gui,
                "conversation-1",
                "message-root",
            )
            .map_err(|error| error.to_string())?;
        let (projector, _) = test_projector(SubagentEventBus::new(), foreground_turns, &temp)?;
        let identity = EventIdentity::new("continuation-stream", "continuation-turn")
            .map_err(|error| error.to_string())?
            .with_conversation_id("conversation-1")
            .map_err(|error| error.to_string())?
            .with_message_id("message-root")
            .map_err(|error| error.to_string())?
            .with_execution_id("continuation-execution")
            .map_err(|error| error.to_string())?;
        let envelope = EventEnvelope::new(
            &identity,
            1,
            None,
            SubagentEventPayload {
                invocation: SubagentInvocationIdentity {
                    parent_agent: "primary".to_string(),
                    agent_name: "explorer".to_string(),
                    parent_execution_id: None,
                    agent_path: Some("primary/explorer".to_string()),
                    task_id: None,
                    attempt: Some(1),
                    plan_revision: None,
                },
                event: SubagentEvent::DispatchStarted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    mode: ExecutionMode::Fork,
                    task: "continue".to_string(),
                    execution_id: Some("continuation-execution".to_string()),
                    run_id: None,
                    conversation_id: Some("conversation-1".to_string()),
                    message_id: Some("message-root".to_string()),
                    background: false,
                },
            },
        )
        .map_err(|error| error.to_string())?;
        let projected = projector
            .ingest(Arc::new(envelope))
            .await
            .map_err(|error| error.to_string())?;
        let event = projected
            .first()
            .ok_or_else(|| "missing continuation projection".to_string())?;
        assert_eq!(event.envelope.root_turn_id, "message-root");
        assert_eq!(event.envelope.turn_id, "continuation-turn");
        assert_eq!(event.envelope.message_id, "message-root");
        Ok(())
    }

    #[tokio::test]
    async fn derived_tool_failure_does_not_duplicate_committed_framework_event()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let foreground_turns = ForegroundTurnControl::default();
        let _turn = foreground_turns
            .begin_scoped(
                "workspace-1",
                ForegroundTurnSurface::Gui,
                "conversation-1",
                "message-1",
            )
            .map_err(|error| error.to_string())?;
        let (projector, chat_events) =
            test_projector(SubagentEventBus::new(), foreground_turns, &temp)?;
        projector
            .ingest(Arc::new(ordinary_envelope(
                1,
                SubagentEvent::DispatchStarted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    mode: ExecutionMode::Fork,
                    task: "inspect".to_string(),
                    execution_id: Some("execution-1".to_string()),
                    run_id: None,
                    conversation_id: Some("conversation-1".to_string()),
                    message_id: Some("message-1".to_string()),
                    background: false,
                },
            )?))
            .await
            .map_err(|error| error.to_string())?;
        let orphan = Arc::new(ordinary_envelope(
            2,
            SubagentEvent::DispatchToolCompleted {
                parent: "primary".to_string(),
                agent: "explorer".to_string(),
                call_id: "missing-call".to_string(),
                name: "read_file".to_string(),
                result: ToolResult::success("done"),
                execution_id: Some("execution-1".to_string()),
                run_id: None,
            },
        )?);
        let projected = projector
            .ingest(orphan.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(projected.len(), 1);
        assert!(
            projected
                .first()
                .is_some_and(|event| event.tool_updates.is_empty())
        );
        assert!(
            projector
                .ingest(orphan)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert_eq!(projector.tool_projector.pending_count(), 1);

        let start = Arc::new(ordinary_envelope(
            3,
            SubagentEvent::DispatchToolStarted {
                parent: "primary".to_string(),
                agent: "explorer".to_string(),
                call_id: "missing-call".to_string(),
                invocation: echo_agent::agent::ToolInvocation {
                    requested_name: "read_file".to_string(),
                    requested_args: serde_json::json!({"path": "README.md"}),
                    name: "read_file".to_string(),
                    args: serde_json::json!({"path": "README.md"}),
                    rewrites: Vec::new(),
                },
                execution_id: Some("execution-1".to_string()),
                run_id: None,
            },
        )?);
        projector
            .ingest(start)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(projector.tool_projector.pending_count(), 1);

        let retry_trigger = Arc::new(ordinary_envelope(
            4,
            SubagentEvent::DispatchTokenDelta {
                parent: "primary".to_string(),
                agent: "explorer".to_string(),
                content: "continue".to_string(),
                execution_id: Some("execution-1".to_string()),
                run_id: None,
            },
        )?);
        let retried = projector
            .ingest(retry_trigger)
            .await
            .map_err(|error| error.to_string())?;
        assert!(retried.first().is_some_and(|event| {
            event.tool_updates.iter().any(|update| {
                update.kind == ToolExecutionProjectionKind::Finished
                    && update.summary.call_id == "missing-call"
            })
        }));
        assert_eq!(projector.tool_projector.pending_count(), 0);
        assert_eq!(
            chat_events
                .replay("workspace-1", Some("conversation-1"), "message-1", 0,)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            4
        );
        Ok(())
    }

    #[tokio::test]
    async fn retained_replay_marks_unrecoverable_transient_gap() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let foreground_turns = ForegroundTurnControl::default();
        let _turn = foreground_turns
            .begin_scoped(
                "workspace-1",
                ForegroundTurnSurface::Gui,
                "conversation-1",
                "message-1",
            )
            .map_err(|error| error.to_string())?;
        let bus = SubagentEventBus::with_capacity(2);
        let (projector, _) = test_projector(bus.clone(), foreground_turns, &temp)?;
        let identity = EventIdentity::new("gap-stream", "message-1")
            .map_err(|error| error.to_string())?
            .with_conversation_id("conversation-1")
            .map_err(|error| error.to_string())?
            .with_message_id("message-1")
            .map_err(|error| error.to_string())?
            .with_execution_id("execution-gap")
            .map_err(|error| error.to_string())?;
        let invocation = SubagentInvocationIdentity {
            parent_agent: "primary".to_string(),
            agent_name: "explorer".to_string(),
            parent_execution_id: None,
            agent_path: Some("primary/explorer".to_string()),
            task_id: None,
            attempt: Some(1),
            plan_revision: None,
        };
        let publisher = bus
            .publisher(identity, invocation)
            .map_err(|error| error.to_string())?;
        publisher
            .emit(SubagentEvent::DispatchStarted {
                parent: "primary".to_string(),
                agent: "explorer".to_string(),
                mode: ExecutionMode::Sync,
                task: "inspect".to_string(),
                execution_id: Some("execution-gap".to_string()),
                run_id: None,
                conversation_id: Some("conversation-1".to_string()),
                message_id: Some("message-1".to_string()),
                background: false,
            })
            .map_err(|error| error.to_string())?;
        for content in ["a", "b", "c", "d"] {
            publisher
                .emit(SubagentEvent::DispatchTokenDelta {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    content: content.to_string(),
                    execution_id: Some("execution-gap".to_string()),
                    run_id: None,
                })
                .map_err(|error| error.to_string())?;
        }
        let terminal = publisher
            .emit(SubagentEvent::DispatchCompleted {
                parent: "primary".to_string(),
                agent: "explorer".to_string(),
                duration_ms: 20,
                tokens_used: Some(10),
                iterations: Some(1),
                output: "abcd".to_string(),
                outcome: SubagentOutcome::terminal(SubagentStatus::Completed, "done", Vec::new()),
                execution_id: Some("execution-gap".to_string()),
                run_id: None,
            })
            .map_err(|error| error.to_string())?;

        let projected = projector
            .ingest(terminal)
            .await
            .map_err(|error| error.to_string())?;
        let kinds = projected
            .iter()
            .filter_map(|event| match &event.envelope.payload {
                ChatDriverEvent::Execution(event) => Some(event.event),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                RuntimeEventKind::Started,
                RuntimeEventKind::SubagentStreamGap,
                RuntimeEventKind::TokenDelta,
                RuntimeEventKind::Completed,
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn service_is_the_single_bus_subscriber_and_publishes_background_commits()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let foreground_turns = ForegroundTurnControl::default();
        let _turn = foreground_turns
            .begin_scoped(
                "workspace-1",
                ForegroundTurnSurface::Gui,
                "conversation-1",
                "message-1",
            )
            .map_err(|error| error.to_string())?;
        let bus = SubagentEventBus::new();
        let (projector, _) = test_projector(bus.clone(), foreground_turns, &temp)?;
        let service = SubagentEnvelopeProjectionService::start(projector);
        let mut committed = service.subscribe_committed();
        assert_eq!(bus.envelope_subscriber_count(), 1);

        let identity = EventIdentity::new("service-stream", "message-1")
            .map_err(|error| error.to_string())?
            .with_conversation_id("conversation-1")
            .map_err(|error| error.to_string())?
            .with_message_id("message-1")
            .map_err(|error| error.to_string())?
            .with_execution_id("service-execution")
            .map_err(|error| error.to_string())?;
        let publisher = bus
            .publisher(
                identity,
                SubagentInvocationIdentity {
                    parent_agent: "primary".to_string(),
                    agent_name: "explorer".to_string(),
                    parent_execution_id: None,
                    agent_path: Some("primary/explorer".to_string()),
                    task_id: None,
                    attempt: Some(1),
                    plan_revision: None,
                },
            )
            .map_err(|error| error.to_string())?;
        publisher
            .emit(SubagentEvent::DispatchStarted {
                parent: "primary".to_string(),
                agent: "explorer".to_string(),
                mode: ExecutionMode::Fork,
                task: "background inspection".to_string(),
                execution_id: Some("service-execution".to_string()),
                run_id: None,
                conversation_id: Some("conversation-1".to_string()),
                message_id: Some("message-1".to_string()),
                background: true,
            })
            .map_err(|error| error.to_string())?;
        let delivered = tokio::time::timeout(std::time::Duration::from_secs(2), committed.recv())
            .await
            .map_err(|_| "timed out waiting for committed projection".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            &delivered.envelope.payload,
            ChatDriverEvent::Execution(event) if event.event == RuntimeEventKind::Started
        ));
        assert!(
            service
                .replay_committed()
                .iter()
                .any(|candidate| { candidate.envelope.event_id == delivered.envelope.event_id })
        );
        service.shutdown_and_join().await?;
        Ok(())
    }

    #[tokio::test]
    async fn lag_recovery_discovers_short_streams_that_were_never_observed() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let foreground_turns = ForegroundTurnControl::default();
        let _turn = foreground_turns
            .begin_scoped(
                "workspace-1",
                ForegroundTurnSurface::Gui,
                "conversation-1",
                "message-1",
            )
            .map_err(|error| error.to_string())?;
        let bus = SubagentEventBus::with_capacity(8);
        let (projector, _) = test_projector(bus.clone(), foreground_turns, &temp)?;
        for (stream_id, execution_id) in [
            ("unseen-stream-a", "unseen-execution-a"),
            ("unseen-stream-b", "unseen-execution-b"),
        ] {
            let identity = EventIdentity::new(stream_id, "message-1")
                .map_err(|error| error.to_string())?
                .with_conversation_id("conversation-1")
                .map_err(|error| error.to_string())?
                .with_message_id("message-1")
                .map_err(|error| error.to_string())?
                .with_execution_id(execution_id)
                .map_err(|error| error.to_string())?;
            let publisher = bus
                .publisher(
                    identity,
                    SubagentInvocationIdentity {
                        parent_agent: "primary".to_string(),
                        agent_name: "explorer".to_string(),
                        parent_execution_id: None,
                        agent_path: Some("primary/explorer".to_string()),
                        task_id: None,
                        attempt: Some(1),
                        plan_revision: None,
                    },
                )
                .map_err(|error| error.to_string())?;
            publisher
                .emit(SubagentEvent::DispatchStarted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    mode: ExecutionMode::Fork,
                    task: "short background task".to_string(),
                    execution_id: Some(execution_id.to_string()),
                    run_id: None,
                    conversation_id: Some("conversation-1".to_string()),
                    message_id: Some("message-1".to_string()),
                    background: true,
                })
                .map_err(|error| error.to_string())?;
            publisher
                .emit(SubagentEvent::DispatchCompleted {
                    parent: "primary".to_string(),
                    agent: "explorer".to_string(),
                    duration_ms: 1,
                    tokens_used: None,
                    iterations: Some(1),
                    output: "done".to_string(),
                    outcome: SubagentOutcome::terminal(
                        SubagentStatus::Completed,
                        "done",
                        Vec::new(),
                    ),
                    execution_id: Some(execution_id.to_string()),
                    run_id: None,
                })
                .map_err(|error| error.to_string())?;
        }

        let recovered = projector
            .recover_known()
            .await
            .map_err(|error| error.to_string())?;
        let executions = recovered
            .iter()
            .filter_map(|projected| match &projected.envelope.payload {
                ChatDriverEvent::Execution(event) => event.subagent_run_id.clone(),
                _ => None,
            })
            .collect::<HashSet<_>>();
        assert_eq!(recovered.len(), 4);
        assert_eq!(
            executions,
            HashSet::from([
                "unseen-execution-a".to_string(),
                "unseen-execution-b".to_string(),
            ])
        );
        Ok(())
    }

    #[tokio::test]
    async fn lag_recovery_addresses_a_fully_evicted_active_stream_from_its_anchor()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let foreground_turns = ForegroundTurnControl::default();
        let _turn = foreground_turns
            .begin_scoped(
                "workspace-1",
                ForegroundTurnSurface::Gui,
                "conversation-1",
                "message-1",
            )
            .map_err(|error| error.to_string())?;
        let bus = SubagentEventBus::with_capacity(1);
        let (projector, _) = test_projector(bus.clone(), foreground_turns, &temp)?;
        let active = bus
            .publisher(
                EventIdentity::new("active-stream", "message-1")
                    .map_err(|error| error.to_string())?
                    .with_conversation_id("conversation-1")
                    .map_err(|error| error.to_string())?
                    .with_message_id("message-1")
                    .map_err(|error| error.to_string())?
                    .with_execution_id("active-execution")
                    .map_err(|error| error.to_string())?,
                SubagentInvocationIdentity {
                    parent_agent: "primary".to_string(),
                    agent_name: "explorer".to_string(),
                    parent_execution_id: None,
                    agent_path: Some("primary/explorer".to_string()),
                    task_id: None,
                    attempt: Some(1),
                    plan_revision: None,
                },
            )
            .map_err(|error| error.to_string())?;
        active
            .emit(SubagentEvent::DispatchStarted {
                parent: "primary".to_string(),
                agent: "explorer".to_string(),
                mode: ExecutionMode::Fork,
                task: "long task".to_string(),
                execution_id: Some("active-execution".to_string()),
                run_id: None,
                conversation_id: Some("conversation-1".to_string()),
                message_id: Some("message-1".to_string()),
                background: true,
            })
            .map_err(|error| error.to_string())?;
        let noisy = bus
            .publisher(
                EventIdentity::new("noisy-stream", "message-1")
                    .map_err(|error| error.to_string())?
                    .with_conversation_id("conversation-1")
                    .map_err(|error| error.to_string())?
                    .with_message_id("message-1")
                    .map_err(|error| error.to_string())?
                    .with_execution_id("noisy-execution")
                    .map_err(|error| error.to_string())?,
                SubagentInvocationIdentity {
                    parent_agent: "primary".to_string(),
                    agent_name: "reviewer".to_string(),
                    parent_execution_id: None,
                    agent_path: Some("primary/reviewer".to_string()),
                    task_id: None,
                    attempt: Some(1),
                    plan_revision: None,
                },
            )
            .map_err(|error| error.to_string())?;
        noisy
            .emit(SubagentEvent::DispatchStarted {
                parent: "primary".to_string(),
                agent: "reviewer".to_string(),
                mode: ExecutionMode::Fork,
                task: "noisy task".to_string(),
                execution_id: Some("noisy-execution".to_string()),
                run_id: None,
                conversation_id: Some("conversation-1".to_string()),
                message_id: Some("message-1".to_string()),
                background: true,
            })
            .map_err(|error| error.to_string())?;

        let recovered = projector
            .recover_known()
            .await
            .map_err(|error| error.to_string())?;
        assert!(recovered.iter().any(|projected| {
            matches!(
                &projected.envelope.payload,
                ChatDriverEvent::Execution(event)
                    if event.subagent_run_id.as_deref() == Some("active-execution")
                        && event.event == RuntimeEventKind::SubagentStreamGap
            )
        }));
        Ok(())
    }
}
