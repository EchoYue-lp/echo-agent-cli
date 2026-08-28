//! Thin EKO adapter over the framework journal authority.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use chrono::{DateTime, Utc};
use echo_agent::state::journal::{
    ApplyBatchReceipt, ApplyReceipt, CheckpointFrame, CheckpointStore, CheckpointedApplyError,
    CheckpointedReducer, EventJournal, FileCheckpointStore, FileEventJournal,
    JournalBatchAppendError, JournalBatchCommitStatus, JournalBatchLookup, JournalDurabilityStatus,
    MemoryCheckpointStore, PreparedJournalBatch,
};
use echo_agent::utils::fs::{FileDurability, atomic_write, create_dir_all_durable};
use serde::{Deserialize, Serialize};

use super::event_rebuild::{
    CompletionGateProjection, EventFoldState, RebuildError, TodoQueryProjection,
};
use super::file_shadow::{ProjectionRefreshStats, ShadowError};
use super::history_projection::{
    HistoryProjection, HistoryProjectionApplyStatus, artifacts_from_events, reviews_from_events,
};
use super::types::{PlanRevision, RunStateSnapshot, RuntimeEventKind, RuntimeTaskEvent};

const REPLAY_BATCH: usize = 512;
const MAX_BATCH_COMMIT_ATTEMPTS: usize = 3;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeJournalEvent {
    run_id: String,
    task_id: Option<String>,
    step_id: Option<String>,
    event_type: RuntimeEventKind,
    payload: serde_json::Value,
    timestamp: DateTime<Utc>,
}

impl RuntimeJournalEvent {
    pub(crate) fn new(
        run_id: impl Into<String>,
        task_id: Option<String>,
        step_id: Option<String>,
        event_type: RuntimeEventKind,
        payload: serde_json::Value,
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            task_id,
            step_id,
            event_type,
            payload,
            timestamp,
        }
    }

    pub(crate) fn for_append(
        run_id: &str,
        task_id: Option<&str>,
        step_id: Option<&str>,
        event_type: RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self::new(
            run_id,
            task_id.map(str::to_string),
            step_id.map(str::to_string),
            event_type,
            payload,
            Utc::now(),
        )
    }

    pub(crate) fn project(&self, sequence: u64) -> Result<RuntimeTaskEvent, u64> {
        let seq = i64::try_from(sequence).map_err(|_| sequence)?;
        Ok(RuntimeTaskEvent {
            seq,
            run_id: self.run_id.clone(),
            task_id: self.task_id.clone(),
            step_id: self.step_id.clone(),
            event_type: self.event_type,
            payload: self.payload.clone(),
            timestamp: self.timestamp,
        })
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(crate) fn event_type(&self) -> RuntimeEventKind {
        self.event_type
    }
}

type RuntimeReducer = CheckpointedReducer<FileEventJournal<RuntimeJournalEvent>, EventFoldState>;

struct RuntimeCheckpointStore {
    file: FileCheckpointStore<EventFoldState>,
    ignore_next_load: std::sync::atomic::AtomicBool,
}

impl RuntimeCheckpointStore {
    fn new(path: &Path, ignore_next_load: bool) -> Self {
        Self {
            file: FileCheckpointStore::open(path),
            ignore_next_load: std::sync::atomic::AtomicBool::new(ignore_next_load),
        }
    }
}

impl CheckpointStore<EventFoldState> for RuntimeCheckpointStore {
    fn save(&self, state: &EventFoldState, through_sequence: u64) -> echo_agent::error::Result<()> {
        self.file.save(state, through_sequence)
    }

    fn load(&self) -> echo_agent::error::Result<Option<CheckpointFrame<EventFoldState>>> {
        if self
            .ignore_next_load
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(None);
        }
        self.file.load()
    }
}

pub(crate) struct RunAuthorityState {
    journal: Arc<FileEventJournal<RuntimeJournalEvent>>,
    checkpoints: Arc<RuntimeCheckpointStore>,
    reducer: RuntimeReducer,
    recovered_from_checkpoint: bool,
    recovery_last_sequence: u64,
    recovery_folded_events: u64,
    projection_sequence: u64,
    durability_debt: Option<String>,
    history: HistoryProjection,
}

pub(crate) struct RunAuthority {
    event_path: PathBuf,
    checkpoint_path: PathBuf,
    expected_run_id: String,
    state: Mutex<Option<RunAuthorityState>>,
    #[cfg(test)]
    fail_next_post_commit_validation: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_durability_settlement: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    reconcile_next_append_unconfirmed: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_durability_probe: std::sync::atomic::AtomicBool,
}

pub(crate) struct RunBatchAppendReceipt {
    pub(crate) events: Vec<Arc<RuntimeTaskEvent>>,
    pub(crate) apply: ApplyBatchReceipt,
    pub(crate) history: HistoryProjectionApplyStatus,
}

enum RegistryStatus {
    Opening,
    Ready(Weak<RunAuthority>),
    Invalidating,
}

struct RegistrySlot {
    status: Mutex<RegistryStatus>,
    changed: Condvar,
}

#[derive(Default)]
struct AuthorityRegistry {
    entries: HashMap<PathBuf, Arc<RegistrySlot>>,
    operations: usize,
}

fn authority_registry() -> &'static Mutex<AuthorityRegistry> {
    static REGISTRY: OnceLock<Mutex<AuthorityRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(AuthorityRegistry::default()))
}

fn prune_registry(registry: &mut AuthorityRegistry) {
    registry.operations = registry.operations.saturating_add(1);
    if !registry.operations.is_multiple_of(32) && registry.entries.len() <= 256 {
        return;
    }
    registry.entries.retain(|_, slot| {
        if Arc::strong_count(slot) > 1 {
            return true;
        }
        let status = slot
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        !matches!(&*status, RegistryStatus::Ready(authority) if authority.strong_count() == 0)
    });
}

fn canonical_event_path(path: &Path, create_parent: bool) -> Result<PathBuf, ShadowError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if create_parent {
        create_dir_all_durable(parent).map_err(|error| ShadowError::Io(error.to_string()))?;
    }
    let parent =
        std::fs::canonicalize(parent).map_err(|error| ShadowError::Io(error.to_string()))?;
    let name = path
        .file_name()
        .ok_or_else(|| ShadowError::Io("TaskRuntime event path has no file name".to_string()))?;
    Ok(parent.join(name))
}

#[cfg(test)]
type RunOpenPause = Arc<(std::sync::Barrier, std::sync::Barrier)>;

#[cfg(test)]
fn open_pauses() -> &'static Mutex<HashMap<PathBuf, RunOpenPause>> {
    static PAUSES: OnceLock<Mutex<HashMap<PathBuf, RunOpenPause>>> = OnceLock::new();
    PAUSES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) struct RunInvalidationGuard {
    slots: Vec<(PathBuf, Arc<RegistrySlot>)>,
}

impl Drop for RunInvalidationGuard {
    fn drop(&mut self) {
        for (_, slot) in &self.slots {
            if let Ok(mut status) = slot.status.lock()
                && matches!(*status, RegistryStatus::Invalidating)
            {
                *status = RegistryStatus::Ready(Weak::new());
                slot.changed.notify_all();
            }
        }
    }
}

impl RunAuthority {
    #[cfg(test)]
    pub(crate) fn lock_operation_for_test(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<RunAuthorityState>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn pause_next_open_for_test(
        path: &Path,
    ) -> Result<Arc<(std::sync::Barrier, std::sync::Barrier)>, ShadowError> {
        let path = canonical_event_path(path, true)?;
        let pause = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        open_pauses()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(path, Arc::clone(&pause));
        Ok(pause)
    }

    #[cfg(test)]
    pub(crate) fn held_lookup_survives_prune_for_test(
        event_path: &Path,
        checkpoint_path: &Path,
        run_id: &str,
    ) -> Result<bool, ShadowError> {
        let authority = Self::open(event_path, checkpoint_path, run_id)?;
        let key = authority.event_path.clone();
        drop(authority);
        let held = {
            let registry = authority_registry().lock().map_err(|error| {
                ShadowError::Io(format!("TaskRuntime authority registry poisoned: {error}"))
            })?;
            registry.entries.get(&key).cloned()
        }
        .ok_or_else(|| ShadowError::Io("run registry slot missing".to_string()))?;
        let retained = {
            let mut registry = authority_registry().lock().map_err(|error| {
                ShadowError::Io(format!("TaskRuntime authority registry poisoned: {error}"))
            })?;
            registry.operations = 31;
            prune_registry(&mut registry);
            registry
                .entries
                .get(&key)
                .is_some_and(|slot| Arc::ptr_eq(slot, &held))
        };
        Ok(retained)
    }

    pub(crate) fn open(
        event_path: &Path,
        checkpoint_path: &Path,
        expected_run_id: &str,
    ) -> Result<Arc<Self>, ShadowError> {
        let event_path = canonical_event_path(event_path, true)?;
        let (slot, mut opener) = {
            let mut registry = authority_registry().lock().map_err(|error| {
                ShadowError::Io(format!("TaskRuntime authority registry poisoned: {error}"))
            })?;
            prune_registry(&mut registry);
            match registry.entries.get(&event_path) {
                Some(slot) => (Arc::clone(slot), false),
                None => {
                    let slot = Arc::new(RegistrySlot {
                        status: Mutex::new(RegistryStatus::Opening),
                        changed: Condvar::new(),
                    });
                    registry
                        .entries
                        .insert(event_path.clone(), Arc::clone(&slot));
                    (slot, true)
                }
            }
        };
        loop {
            if opener {
                let opened = Self::open_state(&event_path, checkpoint_path, expected_run_id)
                    .and_then(|state| {
                        let authority = Arc::new(Self {
                            event_path: event_path.clone(),
                            checkpoint_path: checkpoint_path.to_path_buf(),
                            expected_run_id: expected_run_id.to_string(),
                            state: Mutex::new(Some(state)),
                            #[cfg(test)]
                            fail_next_post_commit_validation: std::sync::atomic::AtomicBool::new(
                                false,
                            ),
                            #[cfg(test)]
                            fail_next_durability_settlement: std::sync::atomic::AtomicBool::new(
                                false,
                            ),
                            #[cfg(test)]
                            reconcile_next_append_unconfirmed: std::sync::atomic::AtomicBool::new(
                                false,
                            ),
                            #[cfg(test)]
                            fail_next_durability_probe: std::sync::atomic::AtomicBool::new(false),
                        });
                        authority.validate_run_id(expected_run_id)?;
                        Ok(authority)
                    });
                let mut status = slot
                    .status
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if matches!(*status, RegistryStatus::Invalidating) {
                    slot.changed.notify_all();
                    return Err(ShadowError::AuthorityClosed(format!(
                        "TaskRuntime run {} is being invalidated",
                        event_path.display()
                    )));
                }
                match opened {
                    Ok(authority) => {
                        *status = RegistryStatus::Ready(Arc::downgrade(&authority));
                        slot.changed.notify_all();
                        return Ok(authority);
                    }
                    Err(error) => {
                        *status = RegistryStatus::Ready(Weak::new());
                        slot.changed.notify_all();
                        return Err(error);
                    }
                }
            }
            let mut status = slot
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match &*status {
                RegistryStatus::Opening => {
                    status = slot
                        .changed
                        .wait(status)
                        .unwrap_or_else(|error| error.into_inner());
                    drop(status);
                }
                RegistryStatus::Invalidating => {
                    return Err(ShadowError::AuthorityClosed(format!(
                        "TaskRuntime run {} is being invalidated",
                        event_path.display()
                    )));
                }
                RegistryStatus::Ready(authority) => {
                    if let Some(authority) = authority.upgrade()
                        && authority.is_open()
                    {
                        drop(status);
                        authority.validate_run_id(expected_run_id)?;
                        return Ok(authority);
                    }
                    *status = RegistryStatus::Opening;
                    opener = true;
                }
            }
        }
    }

    fn open_state(
        event_path: &Path,
        checkpoint_path: &Path,
        expected_run_id: &str,
    ) -> Result<RunAuthorityState, ShadowError> {
        #[cfg(test)]
        let pause = open_pauses()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(event_path);
        #[cfg(test)]
        if let Some(pause) = pause {
            pause.0.wait();
            pause.1.wait();
        }
        let journal = Arc::new(
            FileEventJournal::open(event_path, FileDurability::SyncData)
                .map_err(|error| ShadowError::Rebuild(error.to_string()))?,
        );
        let checkpoint_file = FileCheckpointStore::<EventFoldState>::open(checkpoint_path);
        let journal_last = journal.last_sequence();
        let loaded_checkpoint = checkpoint_file.load().ok().flatten();
        let checkpoint_schema_current = loaded_checkpoint
            .as_ref()
            .is_none_or(|frame| frame.state.has_current_query_projection_schema());
        if !checkpoint_schema_current {
            match std::fs::remove_file(checkpoint_path) {
                Ok(()) => tracing::info!(
                    path = %checkpoint_path.display(),
                    "discarded legacy TaskRuntime checkpoint before rebuilding query projection"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(
                    path = %checkpoint_path.display(),
                    %error,
                    "legacy TaskRuntime checkpoint could not be removed; ignoring it for recovery"
                ),
            }
        }
        let checkpoints = Arc::new(RuntimeCheckpointStore::new(
            checkpoint_path,
            !checkpoint_schema_current,
        ));
        let valid_checkpoint_sequence = loaded_checkpoint
            .filter(|frame| checkpoint_schema_current && frame.sequence <= journal_last)
            .map(|frame| frame.sequence);
        let reducer = CheckpointedReducer::new(
            Arc::clone(&journal),
            Arc::clone(&checkpoints) as Arc<dyn CheckpointStore<EventFoldState>>,
            0,
        );
        let recovery = reducer
            .recover()
            .map_err(|error| ShadowError::Rebuild(error.to_string()))?;
        let checkpoint_behind = valid_checkpoint_sequence
            .is_some_and(|sequence| sequence < recovery.last_applied_sequence);
        if (!checkpoint_schema_current || checkpoint_behind)
            && let Err(error) = reducer.checkpoint()
        {
            tracing::warn!(
                path = %checkpoint_path.display(),
                %error,
                "TaskRuntime checkpoint recovered in memory but atomic repair degraded"
            );
        }
        let recovered_from_checkpoint = valid_checkpoint_sequence.is_some()
            && matches!(
                recovery.checkpoint,
                echo_agent::state::journal::CheckpointRecoveryStatus::Loaded
            );
        let recovery_folded_events = recovery
            .last_applied_sequence
            .saturating_sub(valid_checkpoint_sequence.unwrap_or_default());
        if let echo_agent::state::journal::CheckpointRecoveryStatus::Degraded { reason, error } =
            &recovery.checkpoint
        {
            tracing::warn!(path = %event_path.display(), %reason, %error, "TaskRuntime checkpoint repair is degraded");
        }
        let run_directory = event_path.parent().ok_or_else(|| {
            ShadowError::Io("TaskRuntime journal has no run directory".to_string())
        })?;
        let mut history = HistoryProjection::open(
            expected_run_id,
            run_directory,
            recovery.last_applied_sequence,
        );
        let history_status = reconcile_history_projection(
            &mut history,
            journal.as_ref(),
            recovery.last_applied_sequence,
        );
        if let HistoryProjectionApplyStatus::Degraded { error } = &history_status {
            tracing::warn!(path = %event_path.display(), %error, "TaskRuntime history projection recovery degraded");
        }
        Ok(RunAuthorityState {
            journal,
            checkpoints,
            reducer,
            recovered_from_checkpoint,
            recovery_last_sequence: recovery.last_applied_sequence,
            recovery_folded_events,
            projection_sequence: 0,
            durability_debt: None,
            history,
        })
    }

    #[cfg(test)]
    pub(crate) fn append(
        &self,
        event: RuntimeJournalEvent,
    ) -> Result<
        (
            Arc<RuntimeTaskEvent>,
            ApplyReceipt,
            HistoryProjectionApplyStatus,
        ),
        ShadowError,
    > {
        self.append_with_observer(event, |_| {})
    }

    pub(crate) fn append_with_observer(
        &self,
        event: RuntimeJournalEvent,
        observer: impl FnOnce(&RuntimeTaskEvent),
    ) -> Result<
        (
            Arc<RuntimeTaskEvent>,
            ApplyReceipt,
            HistoryProjectionApplyStatus,
        ),
        ShadowError,
    > {
        let mut observer = Some(observer);
        let batch = self.append_batch_with_observer(vec![event], |event| {
            if let Some(observer) = observer.take() {
                observer(event);
            }
        })?;
        let event = batch.events.into_iter().next().ok_or_else(|| {
            ShadowError::Rebuild("single-event batch committed without a projection".to_string())
        })?;
        Ok((
            event,
            ApplyReceipt {
                batch_id: batch.apply.batch_id,
                sequence: batch.apply.first_sequence,
                journal: batch.apply.journal,
                commit: batch.apply.commit,
                checkpoint: batch.apply.checkpoint,
            },
            batch.history,
        ))
    }

    pub(crate) fn append_batch(
        &self,
        events: Vec<RuntimeJournalEvent>,
    ) -> Result<RunBatchAppendReceipt, ShadowError> {
        self.append_batch_with_observer(events, |_| {})
    }

    pub(crate) fn append_batch_with_observer(
        &self,
        events: Vec<RuntimeJournalEvent>,
        mut observer: impl FnMut(&RuntimeTaskEvent),
    ) -> Result<RunBatchAppendReceipt, ShadowError> {
        let prepared = PreparedJournalBatch::new(events)
            .map_err(|error| ShadowError::Encode(error.to_string()))?;
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let mut prepared = Some(prepared);
        let mut attempts = 0_usize;
        let mut reconciled_projection = None;

        loop {
            attempts = attempts.saturating_add(1);
            let state = guard.as_mut().ok_or_else(|| {
                ShadowError::AuthorityClosed(self.event_path.display().to_string())
            })?;
            Self::retry_durability_debt(state, &self.event_path);
            let batch = prepared.take().ok_or_else(|| {
                ShadowError::Rebuild("prepared TaskRuntime batch ownership was lost".to_string())
            })?;
            let batch_id = batch.batch_id().to_string();
            let payload_digest = batch.payload_digest().to_string();
            let projected = match reconciled_projection.take() {
                Some(projected) => projected,
                None => project_prepared_batch(&batch, state.journal.next_sequence())?,
            };

            let mut receipt = match state.reducer.apply_batch(batch) {
                Ok(receipt) => receipt,
                Err(CheckpointedApplyError::Journal(JournalBatchAppendError::NotCommitted {
                    batch,
                    error,
                })) if attempts < MAX_BATCH_COMMIT_ATTEMPTS => {
                    prepared = Some(batch);
                    tracing::warn!(
                        path = %self.event_path.display(),
                        batch_id = %batch_id,
                        attempt = attempts,
                        %error,
                        "retrying an uncommitted TaskRuntime batch"
                    );
                    continue;
                }
                Err(CheckpointedApplyError::Journal(JournalBatchAppendError::NotCommitted {
                    error,
                    ..
                })) => {
                    let stale = guard.take();
                    drop(stale);
                    return Err(ShadowError::BatchNotCommitted {
                        batch_id,
                        attempts,
                        detail: error,
                    });
                }
                Err(CheckpointedApplyError::Journal(error))
                    if matches!(
                        error,
                        JournalBatchAppendError::OutcomeUnknown { .. }
                            | JournalBatchAppendError::AuthorityPoisoned { .. }
                    ) =>
                {
                    let detail = error.to_string();
                    let batch =
                        error
                            .into_prepared()
                            .ok_or_else(|| ShadowError::BatchOutcomeUnknown {
                                batch_id: batch_id.clone(),
                                payload_digest: payload_digest.clone(),
                                detail: "journal did not return prepared batch ownership"
                                    .to_string(),
                            })?;
                    let stale = guard.take();
                    drop(stale);
                    let reopened = Self::open_state(
                        &self.event_path,
                        &self.checkpoint_path,
                        &self.expected_run_id,
                    )
                    .map_err(|error| ShadowError::BatchOutcomeUnknown {
                        batch_id: batch_id.clone(),
                        payload_digest: payload_digest.clone(),
                        detail: format!("{detail}; verified reopen failed: {error}"),
                    })?;
                    validate_state_run_id(&reopened, &self.expected_run_id)?;
                    match reopened.journal.lookup_batch(&batch).map_err(|error| {
                        ShadowError::BatchOutcomeUnknown {
                            batch_id: batch_id.clone(),
                            payload_digest: payload_digest.clone(),
                            detail: format!("{detail}; batch lookup failed: {error}"),
                        }
                    })? {
                        JournalBatchLookup::AlreadyCommitted(committed) => {
                            reconciled_projection =
                                Some(project_journal_records(committed.records())?);
                            *guard = Some(reopened);
                            prepared = Some(batch);
                            continue;
                        }
                        JournalBatchLookup::Absent if attempts < MAX_BATCH_COMMIT_ATTEMPTS => {
                            *guard = Some(reopened);
                            prepared = Some(batch);
                            continue;
                        }
                        JournalBatchLookup::Absent => {
                            return Err(ShadowError::BatchOutcomeUnknown {
                                batch_id,
                                payload_digest,
                                detail: format!(
                                    "{detail}; batch remained absent after {attempts} attempts"
                                ),
                            });
                        }
                        JournalBatchLookup::Conflict { error } => {
                            drop(reopened);
                            return Err(ShadowError::BatchIdentityConflict {
                                batch_id,
                                payload_digest,
                                detail: error,
                            });
                        }
                    }
                }
                Err(CheckpointedApplyError::Journal(error)) => {
                    let detail = error.to_string();
                    let stale = guard.take();
                    drop(stale);
                    return Err(ShadowError::BatchIdentityConflict {
                        batch_id,
                        payload_digest,
                        detail,
                    });
                }
                Err(CheckpointedApplyError::CommittedInvariant { error, .. }) => {
                    let stale = guard.take();
                    drop(stale);
                    return Err(ShadowError::BatchOutcomeUnknown {
                        batch_id,
                        payload_digest,
                        detail: error,
                    });
                }
                Err(CheckpointedApplyError::Prepare(error)) => {
                    return Err(ShadowError::Encode(error.to_string()));
                }
            };
            #[cfg(test)]
            if self
                .fail_next_post_commit_validation
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                let stale = guard.take();
                drop(stale);
                return Err(ShadowError::BatchOutcomeUnknown {
                    batch_id,
                    payload_digest,
                    detail: "injected post-commit TaskRuntime validation failure".to_string(),
                });
            }
            if let Err(error) = validate_batch_receipt(&receipt, &projected) {
                let detail = error.to_string();
                let stale = guard.take();
                drop(stale);
                return Err(ShadowError::BatchOutcomeUnknown {
                    batch_id,
                    payload_digest,
                    detail,
                });
            }
            match &receipt.journal {
                JournalDurabilityStatus::Confirmed => state.durability_debt = None,
                JournalDurabilityStatus::Unconfirmed => {
                    state.durability_debt = Some(format!(
                        "reconciled TaskRuntime batch {} has unconfirmed durability",
                        receipt.batch_id
                    ));
                    Self::retry_durability_debt(state, &self.event_path);
                    receipt.journal = match state.durability_debt.as_ref() {
                        Some(error) => JournalDurabilityStatus::Degraded {
                            error: error.clone(),
                        },
                        None => JournalDurabilityStatus::Confirmed,
                    };
                }
                JournalDurabilityStatus::Degraded { error } => {
                    state.durability_debt = Some(error.clone());
                    Self::retry_durability_debt(state, &self.event_path);
                    receipt.journal = match state.durability_debt.as_ref() {
                        Some(error) => JournalDurabilityStatus::Degraded {
                            error: error.clone(),
                        },
                        None => JournalDurabilityStatus::Confirmed,
                    };
                }
            }
            #[cfg(test)]
            if self
                .fail_next_durability_settlement
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                let detail = "injected persistent TaskRuntime journal durability debt".to_string();
                state.durability_debt = Some(detail.clone());
                receipt.journal = JournalDurabilityStatus::Degraded { error: detail };
            }
            #[cfg(test)]
            if self
                .reconcile_next_append_unconfirmed
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                state.durability_debt = Some(format!(
                    "reconciled TaskRuntime batch {} has unconfirmed durability",
                    receipt.batch_id
                ));
                receipt.commit = JournalBatchCommitStatus::AlreadyCommitted;
                receipt.journal = JournalDurabilityStatus::Unconfirmed;
            }
            if let Err(error) = state.reducer.with_state(validate_projection_health) {
                let detail = error.to_string();
                let stale = guard.take();
                drop(stale);
                return Err(ShadowError::BatchOutcomeUnknown {
                    batch_id,
                    payload_digest,
                    detail,
                });
            }
            let journal = Arc::clone(&state.journal);
            let journal_head = state.reducer.last_applied_sequence();
            let history =
                reconcile_history_projection(&mut state.history, journal.as_ref(), journal_head);
            for event in &projected {
                observer(event.as_ref());
            }
            return Ok(RunBatchAppendReceipt {
                events: projected,
                apply: receipt,
                history,
            });
        }
    }

    fn retry_durability_debt(state: &mut RunAuthorityState, path: &Path) {
        if state.durability_debt.is_none() {
            return;
        }
        match state.journal.sync_data() {
            Ok(()) => state.durability_debt = None,
            Err(error) => {
                state.durability_debt = Some(error.to_string());
                tracing::warn!(%error, path = %path.display(), "TaskRuntime durability barrier remains pending");
            }
        }
    }

    pub(crate) fn settle_durability_and_history(
        &self,
    ) -> (JournalDurabilityStatus, HistoryProjectionApplyStatus) {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(state) = guard.as_mut() else {
            let error = format!(
                "TaskRuntime authority is closed: {}",
                self.event_path.display()
            );
            return (
                JournalDurabilityStatus::Degraded {
                    error: error.clone(),
                },
                HistoryProjectionApplyStatus::Degraded { error },
            );
        };
        let journal =
            if state.durability_debt.is_none() {
                JournalDurabilityStatus::Confirmed
            } else {
                #[cfg(test)]
                let fail_probe = self
                    .fail_next_durability_probe
                    .swap(false, std::sync::atomic::Ordering::SeqCst);
                #[cfg(not(test))]
                let fail_probe = false;
                if fail_probe {
                    JournalDurabilityStatus::Degraded {
                        error: state.durability_debt.clone().unwrap_or_else(|| {
                            "injected TaskRuntime durability probe failure".to_string()
                        }),
                    }
                } else {
                    Self::retry_durability_debt(state, &self.event_path);
                    state.durability_debt.as_ref().map_or(
                        JournalDurabilityStatus::Confirmed,
                        |error| JournalDurabilityStatus::Degraded {
                            error: error.clone(),
                        },
                    )
                }
            };
        let journal_handle = Arc::clone(&state.journal);
        let journal_head = state.reducer.last_applied_sequence();
        let history =
            reconcile_history_projection(&mut state.history, journal_handle.as_ref(), journal_head);
        (journal, history)
    }

    fn validate_run_id(&self, expected_run_id: &str) -> Result<(), ShadowError> {
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        validate_state_run_id(state, expected_run_id)
    }

    pub(crate) fn replay_after(
        &self,
        after_sequence: u64,
    ) -> Result<Vec<RuntimeTaskEvent>, ShadowError> {
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        replay_journal(state.journal.as_ref(), after_sequence)
    }

    /// Replay at most `limit` records directly from the journal. Unlike
    /// [`Self::replay_after`], this is a single bounded journal read and does
    /// not materialize the complete suffix before returning.
    pub(crate) fn replay_after_bounded(
        &self,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<RuntimeTaskEvent>, ShadowError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        let records = state
            .journal
            .replay_after(after_sequence, limit)
            .map_err(|error| ShadowError::Rebuild(error.to_string()))?;
        records
            .into_iter()
            .map(|record| {
                record
                    .event
                    .project(record.sequence)
                    .map_err(|sequence| ShadowError::SequenceOutOfRange { sequence })
            })
            .collect()
    }

    pub(crate) fn read_plan_projection(&self) -> Result<Option<PlanRevision>, ShadowError> {
        self.read_projection("plan.json")
    }

    pub(crate) fn read_run_state_projection(
        &self,
    ) -> Result<Option<RunStateSnapshot>, ShadowError> {
        self.read_projection("run-state.json")
    }

    pub(crate) fn read_todo_query_projection(
        &self,
    ) -> Result<Option<TodoQueryProjection>, ShadowError> {
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        state.reducer.with_state(|projection| {
            validate_projection_health(projection)?;
            match projection.todo_query_projection() {
                Ok(snapshot) => Ok(Some(snapshot)),
                Err(RebuildError::NoRunCreated) => Ok(None),
            }
        })
    }

    pub(crate) fn read_completion_gate_projection(
        &self,
    ) -> Result<Option<CompletionGateProjection>, ShadowError> {
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        state.reducer.with_state(|projection| {
            validate_projection_health(projection)?;
            match projection.completion_gate_projection() {
                Ok(snapshot) => Ok(Some(snapshot)),
                Err(RebuildError::NoRunCreated) => Ok(None),
            }
        })
    }

    pub(crate) fn read_artifacts_projection(
        &self,
    ) -> Result<Vec<super::types::Artifact>, ShadowError> {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_mut()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        let journal = Arc::clone(&state.journal);
        let journal_head = state.reducer.last_applied_sequence();
        if let Some(artifacts) = state.history.cached_artifacts(journal_head) {
            return Ok(artifacts);
        }
        if let HistoryProjectionApplyStatus::Degraded { error } =
            reconcile_history_projection(&mut state.history, journal.as_ref(), journal_head)
        {
            tracing::warn!(path = %self.event_path.display(), %error, "artifact history repair degraded");
        }
        let suffix = replay_journal(journal.as_ref(), state.history.through_sequence())?;
        match state.history.artifacts_with_suffix(&suffix) {
            Ok(artifacts) => Ok(artifacts),
            Err(error) => {
                #[cfg(test)]
                state.history.record_fallback_replay_for_test();
                let all = replay_journal(journal.as_ref(), 0)?;
                let artifacts = artifacts_from_events(&all);
                let repaired = match state.history.replace_artifacts(&all) {
                    Ok(_) => {
                        if let HistoryProjectionApplyStatus::Degraded { error: repair } =
                            reconcile_history_projection(
                                &mut state.history,
                                journal.as_ref(),
                                journal_head,
                            )
                        {
                            tracing::warn!(path = %self.event_path.display(), %error, %repair, "artifact history cursor repair degraded");
                            false
                        } else {
                            true
                        }
                    }
                    Err(repair) => {
                        tracing::warn!(path = %self.event_path.display(), %error, %repair, "artifact history fallback repair degraded");
                        false
                    }
                };
                if !repaired {
                    state
                        .history
                        .cache_artifacts(journal_head, artifacts.clone());
                }
                Ok(artifacts)
            }
        }
    }

    pub(crate) fn read_reviews_projection(
        &self,
        task_id: &str,
    ) -> Result<Vec<super::types::ReviewResult>, ShadowError> {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_mut()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        let journal = Arc::clone(&state.journal);
        let journal_head = state.reducer.last_applied_sequence();
        if let Some(reviews) = state.history.cached_reviews(task_id, journal_head) {
            return Ok(reviews);
        }
        if let HistoryProjectionApplyStatus::Degraded { error } =
            reconcile_history_projection(&mut state.history, journal.as_ref(), journal_head)
        {
            tracing::warn!(path = %self.event_path.display(), task_id, %error, "review history repair degraded");
        }
        let suffix = replay_journal(journal.as_ref(), state.history.through_sequence())?;
        match state.history.reviews_with_suffix(task_id, &suffix) {
            Ok(reviews) => Ok(reviews),
            Err(error) => {
                #[cfg(test)]
                state.history.record_fallback_replay_for_test();
                let all = replay_journal(journal.as_ref(), 0)?;
                let reviews = reviews_from_events(task_id, &all);
                let repaired = match state.history.replace_reviews(task_id, &all) {
                    Ok(_) => {
                        if let HistoryProjectionApplyStatus::Degraded { error: repair } =
                            reconcile_history_projection(
                                &mut state.history,
                                journal.as_ref(),
                                journal_head,
                            )
                        {
                            tracing::warn!(path = %self.event_path.display(), task_id, %error, %repair, "review history cursor repair degraded");
                            false
                        } else {
                            true
                        }
                    }
                    Err(repair) => {
                        tracing::warn!(path = %self.event_path.display(), task_id, %error, %repair, "review history fallback repair degraded");
                        false
                    }
                };
                if !repaired {
                    state
                        .history
                        .cache_reviews(task_id, journal_head, reviews.clone());
                }
                Ok(reviews)
            }
        }
    }

    pub(crate) fn read_summary_projection(
        &self,
        task_id: &str,
    ) -> Result<Option<super::types::TaskExecutionSummary>, ShadowError> {
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        state.reducer.with_state(|projection| {
            validate_projection_health(projection)?;
            Ok(projection.summary_projection(task_id))
        })
    }

    fn read_projection<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<T>, ShadowError> {
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if guard.is_none() {
            return Err(ShadowError::AuthorityClosed(
                self.event_path.display().to_string(),
            ));
        }
        let path = self
            .event_path
            .parent()
            .ok_or_else(|| ShadowError::Io("TaskRuntime run directory missing".to_string()))?
            .join(name);
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| ShadowError::Decode(error.to_string())),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(ShadowError::Io(error.to_string())),
        }
    }

    pub(crate) fn diagnostic_full_replay(&self) -> Result<RunStateSnapshot, ShadowError> {
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        let checkpoints = Arc::new(MemoryCheckpointStore::<EventFoldState>::new());
        let reducer = CheckpointedReducer::new(
            Arc::clone(&state.journal),
            checkpoints as Arc<dyn CheckpointStore<EventFoldState>>,
            0,
        );
        reducer
            .recover()
            .map_err(|error| ShadowError::Rebuild(error.to_string()))?;
        let sequence = reducer.last_applied_sequence();
        reducer.with_state(|projection| {
            validate_projection_health(projection)?;
            projection
                .rebuilt_plan()
                .map(|rebuilt| rebuilt.run_state_with_sequence(sequence))
                .map_err(|error| ShadowError::Rebuild(error.to_string()))
        })
    }

    pub(crate) fn refresh_projections(
        &self,
        skip_when_current: bool,
    ) -> Result<ProjectionRefreshStats, ShadowError> {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_mut()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        let previous = state.projection_sequence;
        let current = state.reducer.last_applied_sequence();
        if skip_when_current && previous == current {
            return projection_stats(state, previous, current);
        }
        let run_directory = self.event_path.parent().ok_or_else(|| {
            ShadowError::Io("TaskRuntime journal has no run directory".to_string())
        })?;
        state.reducer.with_state(|projection| {
            validate_projection_health(projection)?;
            let rebuilt = match projection.rebuilt_plan() {
                Ok(rebuilt) => rebuilt,
                Err(RebuildError::NoRunCreated) => return Ok(()),
            };
            create_dir_all_durable(run_directory)
                .map_err(|error| projection_degraded(current, error))?;
            if projection.has_committed_plan() {
                let plan = serde_json::to_vec_pretty(&rebuilt.plan_revision())
                    .map_err(|error| projection_degraded(current, error))?;
                atomic_write(&run_directory.join("plan.json"), &plan)
                    .map_err(|error| projection_degraded(current, error))?;
            }
            let run_state = serde_json::to_vec_pretty(&rebuilt.run_state_with_sequence(current))
                .map_err(|error| projection_degraded(current, error))?;
            atomic_write(&run_directory.join("run-state.json"), &run_state)
                .map_err(|error| projection_degraded(current, error))?;
            state
                .checkpoints
                .save(projection, current)
                .map_err(|error| projection_degraded(current, error))
        })?;
        state.projection_sequence = current;
        projection_stats(state, previous, current)
    }

    pub(crate) fn is_open(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.is_some())
            .unwrap_or(false)
    }

    pub(crate) fn cache_evictable(&self) -> bool {
        self.state
            .try_lock()
            .map(|state| {
                state
                    .as_ref()
                    .is_none_or(|state| state.durability_debt.is_none())
            })
            .unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) fn mark_durability_debt_for_test(&self) {
        if let Ok(mut guard) = self.state.lock()
            && let Some(state) = guard.as_mut()
        {
            state.durability_debt = Some("injected durability debt".to_string());
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_post_commit_validation_for_test(&self) {
        self.fail_next_post_commit_validation
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_durability_settlement_for_test(&self) {
        self.fail_next_durability_settlement
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn reconcile_next_append_unconfirmed_for_test(&self) {
        self.reconcile_next_append_unconfirmed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_durability_probe_for_test(&self) {
        self.fail_next_durability_probe
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_review_history_append_for_test(&self) {
        if let Ok(mut guard) = self.state.lock()
            && let Some(state) = guard.as_mut()
        {
            state.history.fail_next_review_append_for_test();
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_history_cursor_writes_for_test(&self, count: usize) {
        if let Ok(mut guard) = self.state.lock()
            && let Some(state) = guard.as_mut()
        {
            state.history.fail_cursor_writes_for_test(count);
        }
    }

    #[cfg(test)]
    pub(crate) fn history_paths_for_test(&self, task_id: &str) -> (PathBuf, PathBuf, PathBuf) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|state| state.history.paths_for_test(task_id))
            .unwrap_or_else(|| {
                let directory = self.event_path.parent().unwrap_or_else(|| Path::new("."));
                (
                    directory.join("artifact-history.jsonl"),
                    directory.join("review-history/closed.jsonl"),
                    directory.join("history-cursor.json"),
                )
            })
    }

    #[cfg(test)]
    pub(crate) fn history_stats_for_test(&self) -> (usize, u64) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map_or((0, 0), |state| state.history.stats_for_test())
    }

    #[cfg(test)]
    pub(crate) fn history_fallback_replay_count_for_test(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map_or(0, |state| state.history.fallback_replay_count_for_test())
    }

    pub(crate) fn begin_invalidate(path: &Path) -> Result<RunInvalidationGuard, ShadowError> {
        let path = canonical_event_path(path, false)?;
        Self::begin_invalidate_paths(vec![path])
    }

    pub(crate) fn begin_invalidate_root(root: &Path) -> Result<RunInvalidationGuard, ShadowError> {
        let root =
            std::fs::canonicalize(root).map_err(|error| ShadowError::Io(error.to_string()))?;
        let paths = {
            let mut registry = authority_registry().lock().map_err(|error| {
                ShadowError::Io(format!("TaskRuntime authority registry poisoned: {error}"))
            })?;
            prune_registry(&mut registry);
            let mut paths = registry
                .entries
                .keys()
                .filter(|path| path.starts_with(&root))
                .cloned()
                .collect::<Vec<_>>();
            paths.sort();
            paths
        };
        Self::begin_invalidate_paths(paths)
    }

    fn begin_invalidate_paths(paths: Vec<PathBuf>) -> Result<RunInvalidationGuard, ShadowError> {
        let slots = {
            let mut registry = authority_registry().lock().map_err(|error| {
                ShadowError::Io(format!("TaskRuntime authority registry poisoned: {error}"))
            })?;
            prune_registry(&mut registry);
            paths
                .into_iter()
                .map(|path| {
                    let slot = registry
                        .entries
                        .entry(path.clone())
                        .or_insert_with(|| {
                            Arc::new(RegistrySlot {
                                status: Mutex::new(RegistryStatus::Ready(Weak::new())),
                                changed: Condvar::new(),
                            })
                        })
                        .clone();
                    (path, slot)
                })
                .collect::<Vec<_>>()
        };
        let mut statuses = Vec::with_capacity(slots.len());
        for (_, slot) in &slots {
            let mut status = slot
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            while matches!(*status, RegistryStatus::Opening) {
                status = slot
                    .changed
                    .wait(status)
                    .unwrap_or_else(|error| error.into_inner());
            }
            statuses.push(status);
        }
        if let Some((index, _)) = statuses
            .iter()
            .enumerate()
            .find(|(_, status)| matches!(&***status, RegistryStatus::Invalidating))
        {
            let path = slots
                .get(index)
                .map(|(path, _)| path.as_path())
                .unwrap_or_else(|| Path::new("unknown"));
            return Err(ShadowError::AuthorityClosed(format!(
                "TaskRuntime authority {} is already invalidating",
                path.display()
            )));
        }
        let mut authorities = Vec::new();
        for status in &mut statuses {
            if let RegistryStatus::Ready(authority) = &**status
                && let Some(authority) = authority.upgrade()
            {
                authorities.push(authority);
            }
            **status = RegistryStatus::Invalidating;
        }
        drop(statuses);
        for authority in &authorities {
            let state = authority
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            drop(state);
        }
        Ok(RunInvalidationGuard { slots })
    }
}

fn reconcile_history_projection(
    history: &mut HistoryProjection,
    journal: &FileEventJournal<RuntimeJournalEvent>,
    journal_head: u64,
) -> HistoryProjectionApplyStatus {
    let after_sequence = if history.needs_full_rebuild() {
        0
    } else {
        history.through_sequence()
    };
    let events = match replay_journal(journal, after_sequence) {
        Ok(events) => events,
        Err(error) => {
            return HistoryProjectionApplyStatus::Degraded {
                error: error.to_string(),
            };
        }
    };
    if history.needs_full_rebuild() {
        history.rebuild_all(&events, journal_head)
    } else {
        let status = history.apply_events(&events, journal_head);
        if !history.needs_full_rebuild() {
            return status;
        }
        let all = match replay_journal(journal, 0) {
            Ok(events) => events,
            Err(error) => {
                return HistoryProjectionApplyStatus::Degraded {
                    error: error.to_string(),
                };
            }
        };
        history.rebuild_all(&all, journal_head)
    }
}

fn validate_state_run_id(
    state: &RunAuthorityState,
    expected_run_id: &str,
) -> Result<(), ShadowError> {
    state.reducer.with_state(|projection| {
        validate_projection_health(projection)?;
        if projection
            .seen_run_ids()
            .iter()
            .any(|run_id| run_id != expected_run_id)
        {
            return Err(ShadowError::Rebuild(format!(
                "TaskRuntime journal contains an event for a run other than {expected_run_id}"
            )));
        }
        Ok(())
    })
}

fn project_prepared_batch(
    batch: &PreparedJournalBatch<RuntimeJournalEvent>,
    first_sequence: u64,
) -> Result<Vec<Arc<RuntimeTaskEvent>>, ShadowError> {
    let count = u64::try_from(batch.len()).map_err(|_| ShadowError::SequenceCapacityExceeded {
        next_sequence: first_sequence,
    })?;
    let next_sequence =
        first_sequence
            .checked_add(count)
            .ok_or(ShadowError::SequenceCapacityExceeded {
                next_sequence: first_sequence,
            })?;
    if first_sequence == 0 || next_sequence.saturating_sub(1) > i64::MAX as u64 {
        return Err(ShadowError::SequenceCapacityExceeded {
            next_sequence: first_sequence,
        });
    }
    batch
        .events()
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let index =
                u64::try_from(index).map_err(|_| ShadowError::SequenceCapacityExceeded {
                    next_sequence: first_sequence,
                })?;
            let sequence =
                first_sequence
                    .checked_add(index)
                    .ok_or(ShadowError::SequenceCapacityExceeded {
                        next_sequence: first_sequence,
                    })?;
            event
                .project(sequence)
                .map(Arc::new)
                .map_err(|sequence| ShadowError::SequenceOutOfRange { sequence })
        })
        .collect()
}

fn project_journal_records(
    records: &[echo_agent::state::journal::JournalRecord<RuntimeJournalEvent>],
) -> Result<Vec<Arc<RuntimeTaskEvent>>, ShadowError> {
    records
        .iter()
        .map(|record| {
            record
                .event
                .project(record.sequence)
                .map(Arc::new)
                .map_err(|sequence| ShadowError::SequenceOutOfRange { sequence })
        })
        .collect()
}

fn validate_batch_receipt(
    receipt: &ApplyBatchReceipt,
    projected: &[Arc<RuntimeTaskEvent>],
) -> Result<(), ShadowError> {
    let projected_count = u64::try_from(projected.len()).map_err(|_| {
        ShadowError::Rebuild("TaskRuntime projected batch count exceeds u64".to_string())
    })?;
    let first = projected.first().ok_or_else(|| {
        ShadowError::Rebuild("TaskRuntime batch committed without projections".to_string())
    })?;
    let last = projected.last().ok_or_else(|| {
        ShadowError::Rebuild("TaskRuntime batch committed without projections".to_string())
    })?;
    let first_sequence =
        u64::try_from(first.seq).map_err(|_| ShadowError::SequenceOutOfRange { sequence: 0 })?;
    let last_sequence =
        u64::try_from(last.seq).map_err(|_| ShadowError::SequenceOutOfRange { sequence: 0 })?;
    if receipt.record_count != projected_count
        || receipt.first_sequence != first_sequence
        || receipt.last_sequence != last_sequence
    {
        return Err(ShadowError::Rebuild(format!(
            "journal batch receipt {}..={} ({} records) does not match EKO projection {}..={} ({} records)",
            receipt.first_sequence,
            receipt.last_sequence,
            receipt.record_count,
            first_sequence,
            last_sequence,
            projected_count
        )));
    }
    if receipt.commit == JournalBatchCommitStatus::AlreadyCommitted
        && receipt.journal == JournalDurabilityStatus::Unconfirmed
    {
        return Ok(());
    }
    Ok(())
}

fn validate_projection_health(projection: &EventFoldState) -> Result<(), ShadowError> {
    if projection.missing_record_sequence() {
        return Err(ShadowError::Rebuild(
            "TaskRuntime reducer was invoked without a journal record sequence".to_string(),
        ));
    }
    if let Some(sequence) = projection.sequence_overflow() {
        return Err(ShadowError::SequenceOutOfRange { sequence });
    }
    Ok(())
}

fn replay_journal(
    journal: &FileEventJournal<RuntimeJournalEvent>,
    after_sequence: u64,
) -> Result<Vec<RuntimeTaskEvent>, ShadowError> {
    let mut cursor = after_sequence;
    let mut events = Vec::new();
    loop {
        let records = journal
            .replay_after(cursor, REPLAY_BATCH)
            .map_err(|error| ShadowError::Rebuild(error.to_string()))?;
        if records.is_empty() {
            break;
        }
        let count = records.len();
        for record in records {
            cursor = record.sequence;
            events.push(
                record
                    .event
                    .project(record.sequence)
                    .map_err(|sequence| ShadowError::SequenceOutOfRange { sequence })?,
            );
        }
        if count < REPLAY_BATCH {
            break;
        }
    }
    Ok(events)
}

fn projection_stats(
    state: &RunAuthorityState,
    previous: u64,
    current: u64,
) -> Result<ProjectionRefreshStats, ShadowError> {
    let folded = if previous == 0 {
        state
            .recovery_folded_events
            .saturating_add(current.saturating_sub(state.recovery_last_sequence))
    } else {
        current.saturating_sub(previous)
    };
    Ok(ProjectionRefreshStats {
        used_checkpoint: state.recovered_from_checkpoint || previous != 0,
        folded_events: usize::try_from(folded).unwrap_or(usize::MAX),
        seq: i64::try_from(current)
            .map_err(|_| ShadowError::SequenceOutOfRange { sequence: current })?,
    })
}

fn projection_degraded(sequence: u64, error: impl std::fmt::Display) -> ShadowError {
    match i64::try_from(sequence) {
        Ok(seq) => ShadowError::CommittedProjectionDegraded {
            seq,
            detail: error.to_string(),
        },
        Err(_) => ShadowError::SequenceOutOfRange { sequence },
    }
}
