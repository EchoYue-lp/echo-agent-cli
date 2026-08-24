//! Thin EKO adapter over the framework journal authority.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use chrono::{DateTime, Utc};
use echo_agent::state::journal::{
    ApplyReceipt, CheckpointStore, CheckpointedReducer, EventJournal, FileCheckpointStore,
    FileEventJournal, JournalDurabilityStatus, MemoryCheckpointStore,
};
use echo_agent::utils::fs::{FileDurability, atomic_write, create_dir_all_durable};
use serde::{Deserialize, Serialize};

use super::event_rebuild::{EventFoldState, RebuildError};
use super::file_shadow::{ProjectionRefreshStats, ShadowError};
use super::types::{PlanRevision, RunStateSnapshot, RuntimeEventKind, RuntimeTaskEvent};

const REPLAY_BATCH: usize = 512;

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

pub(crate) struct RunAuthorityState {
    journal: Arc<FileEventJournal<RuntimeJournalEvent>>,
    checkpoints: Arc<FileCheckpointStore<EventFoldState>>,
    reducer: RuntimeReducer,
    recovered_from_checkpoint: bool,
    recovery_last_sequence: u64,
    recovery_folded_events: u64,
    projection_sequence: u64,
    durability_debt: Option<String>,
}

pub(crate) struct RunAuthority {
    event_path: PathBuf,
    state: Mutex<Option<RunAuthorityState>>,
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
                let opened = Self::open_state(&event_path, checkpoint_path).and_then(|state| {
                    let authority = Arc::new(Self {
                        event_path: event_path.clone(),
                        state: Mutex::new(Some(state)),
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
        let checkpoints = Arc::new(FileCheckpointStore::open(checkpoint_path));
        let journal_last = journal.last_sequence();
        let valid_checkpoint_sequence = checkpoints
            .load()
            .ok()
            .flatten()
            .map(|frame| frame.sequence)
            .filter(|sequence| *sequence <= journal_last);
        let reducer = CheckpointedReducer::new(
            Arc::clone(&journal),
            Arc::clone(&checkpoints) as Arc<dyn CheckpointStore<EventFoldState>>,
            0,
        );
        let recovery = reducer
            .recover()
            .map_err(|error| ShadowError::Rebuild(error.to_string()))?;
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
        Ok(RunAuthorityState {
            journal,
            checkpoints,
            reducer,
            recovered_from_checkpoint,
            recovery_last_sequence: recovery.last_applied_sequence,
            recovery_folded_events,
            projection_sequence: 0,
            durability_debt: None,
        })
    }

    pub(crate) fn append(
        &self,
        event: RuntimeJournalEvent,
    ) -> Result<(Arc<RuntimeTaskEvent>, ApplyReceipt), ShadowError> {
        self.append_with_observer(event, |_| {})
    }

    pub(crate) fn append_with_observer(
        &self,
        event: RuntimeJournalEvent,
        observer: impl FnOnce(&RuntimeTaskEvent),
    ) -> Result<(Arc<RuntimeTaskEvent>, ApplyReceipt), ShadowError> {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_mut()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
        let next_sequence = state.journal.next_sequence();
        if next_sequence > i64::MAX as u64 {
            return Err(ShadowError::SequenceCapacityExceeded { next_sequence });
        }
        Self::retry_durability_debt(state, &self.event_path);
        let receipt = match state.reducer.apply(event) {
            Ok(receipt) => receipt,
            Err(error) => {
                let error = ShadowError::Io(error.to_string());
                let stale = guard.take();
                drop(stale);
                return Err(error);
            }
        };
        let runtime_event = state.reducer.with_state(|projection| {
            validate_projection_health(projection)?;
            projection.last_projected_event().ok_or_else(|| {
                ShadowError::Rebuild(format!(
                    "journal sequence {} committed without an EKO event projection",
                    receipt.sequence
                ))
            })
        })?;
        let expected =
            i64::try_from(receipt.sequence).map_err(|_| ShadowError::SequenceOutOfRange {
                sequence: receipt.sequence,
            })?;
        if runtime_event.seq != expected {
            return Err(ShadowError::Rebuild(format!(
                "journal receipt sequence {} does not match projected event sequence {}",
                receipt.sequence, runtime_event.seq
            )));
        }
        match &receipt.journal {
            JournalDurabilityStatus::Unconfirmed => {
                Self::retry_durability_debt(state, &self.event_path);
            }
            JournalDurabilityStatus::Confirmed => state.durability_debt = None,
            JournalDurabilityStatus::Degraded { error } => {
                state.durability_debt = Some(error.clone());
                Self::retry_durability_debt(state, &self.event_path);
            }
        }
        observer(runtime_event.as_ref());
        Ok((runtime_event, receipt))
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

    fn validate_run_id(&self, expected_run_id: &str) -> Result<(), ShadowError> {
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| ShadowError::AuthorityClosed(self.event_path.display().to_string()))?;
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

    pub(crate) fn read_plan_projection(&self) -> Result<Option<PlanRevision>, ShadowError> {
        self.read_projection("plan.json")
    }

    pub(crate) fn read_run_state_projection(
        &self,
    ) -> Result<Option<RunStateSnapshot>, ShadowError> {
        self.read_projection("run-state.json")
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
        reducer.with_state(|projection| {
            validate_projection_health(projection)?;
            projection
                .rebuilt_plan()
                .map(|rebuilt| rebuilt.run_state())
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
            let run_state = serde_json::to_vec_pretty(&rebuilt.run_state())
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
