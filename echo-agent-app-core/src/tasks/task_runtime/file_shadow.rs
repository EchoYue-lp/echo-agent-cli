//! EKO's TaskRuntime file layout and product projection adapter.
//!
//! `events.jsonl` is the sole fact source. Sequencing, replay, crash-tail
//! repair, the process lease, and checkpoint recovery are delegated to the
//! framework journal through one canonical [`RunAuthority`] per run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use super::root_authority::RootTransactionAuthority;
use super::run_authority::{RunAuthority, RunBatchAppendReceipt, RuntimeJournalEvent};
use super::types::{PlanRevision, RunStateSnapshot, RuntimeEventKind, RuntimeTaskEvent};

const MAX_CACHED_RUN_AUTHORITIES: usize = 128;

#[cfg(test)]
type TestPause = Arc<(std::sync::Barrier, std::sync::Barrier)>;

struct CachedRunAuthority {
    authority: Arc<RunAuthority>,
    last_used: u64,
}

struct ShadowGenerationState {
    root: PathBuf,
    generation: u64,
    transitioning: bool,
    root_authority: Option<RootTransactionAuthority>,
    authorities: HashMap<String, CachedRunAuthority>,
    access_clock: u64,
}

/// EKO product storage rooted at one workspace's TaskRuntime directory.
#[derive(Clone)]
pub(crate) struct FileTaskShadow {
    state: Arc<Mutex<ShadowGenerationState>>,
    #[allow(clippy::type_complexity)]
    event_hook: Arc<OnceLock<Arc<dyn Fn(&RuntimeTaskEvent) + Send + Sync>>>,
    #[cfg(test)]
    fail_initial_publish_before_rename: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    fail_initial_batch_durability: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    deletion_pause: Arc<Mutex<Option<TestPause>>>,
    #[cfg(test)]
    rebind_pause: Arc<Mutex<Option<TestPause>>>,
    #[cfg(test)]
    fail_root_sync_on_call: Arc<std::sync::atomic::AtomicUsize>,
}

impl FileTaskShadow {
    pub(crate) fn try_new(root: impl Into<PathBuf>) -> Result<Self, ShadowError> {
        let root = root.into();
        let root_authority = RootTransactionAuthority::open(&root)?;
        let root = root_authority.root().to_path_buf();
        let shadow = Self {
            state: Arc::new(Mutex::new(ShadowGenerationState {
                root,
                generation: 0,
                transitioning: false,
                root_authority: Some(root_authority),
                authorities: HashMap::new(),
                access_clock: 0,
            })),
            event_hook: Arc::new(OnceLock::new()),
            #[cfg(test)]
            fail_initial_publish_before_rename: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            fail_initial_batch_durability: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            deletion_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            rebind_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            fail_root_sync_on_call: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        shadow.with_root_write(cleanup_transactions)?;
        Ok(shadow)
    }

    #[cfg(test)]
    pub(crate) fn new(root: impl Into<PathBuf>) -> Result<Self, ShadowError> {
        Self::try_new(root)
    }

    #[cfg(test)]
    fn new_unbound_for_test(root: impl Into<PathBuf>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ShadowGenerationState {
                root: root.into(),
                generation: 0,
                transitioning: false,
                root_authority: None,
                authorities: HashMap::new(),
                access_clock: 0,
            })),
            event_hook: Arc::new(OnceLock::new()),
            fail_initial_publish_before_rename: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            fail_initial_batch_durability: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            deletion_pause: Arc::new(Mutex::new(None)),
            rebind_pause: Arc::new(Mutex::new(None)),
            fail_root_sync_on_call: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub(crate) fn try_attach_event_hook(
        &self,
        hook: Arc<dyn Fn(&RuntimeTaskEvent) + Send + Sync>,
    ) -> bool {
        self.event_hook.set(hook).is_ok()
    }

    pub(crate) fn default_root() -> PathBuf {
        crate::data_root::user_data_path("tasks")
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .root
            .clone()
    }

    pub(crate) fn rebind_root(&self, root: PathBuf) -> Result<(), ShadowError> {
        let old_authority = self.root_authority()?;
        let new_authority = RootTransactionAuthority::open(&root)?;
        let (old_root, generation) = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.transitioning {
                return Err(ShadowError::RootTransition);
            }
            state.transitioning = true;
            (state.root.clone(), state.generation)
        };
        let result = (|| {
            let mut roots = vec![old_authority.clone(), new_authority.clone()];
            roots.sort_by(|left, right| left.root().cmp(right.root()));
            roots.dedup_by(|left, right| left.same_authority(right));
            let root_guards = roots
                .iter()
                .map(|authority| authority.write_operation())
                .collect::<Vec<_>>();
            #[cfg(test)]
            if let Some(pause) = self
                .rebind_pause
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                pause.0.wait();
                pause.1.wait();
            }
            let invalidation = if old_root.exists() {
                Some(RunAuthority::begin_invalidate_root(&old_root)?)
            } else {
                None
            };
            cleanup_transactions(new_authority.root())?;
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if !state.transitioning || state.generation != generation {
                return Err(ShadowError::RootTransition);
            }
            state.authorities.clear();
            state.root = new_authority.root().to_path_buf();
            state.root_authority = Some(new_authority);
            state.generation = state.generation.saturating_add(1);
            state.transitioning = false;
            drop(state);
            drop(invalidation);
            drop(root_guards);
            Ok(())
        })();
        if result.is_err() {
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .transitioning = false;
        }
        result
    }

    #[cfg(test)]
    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.root().join(run_id)
    }

    #[cfg(test)]
    fn events_path(&self, run_id: &str) -> PathBuf {
        self.run_dir(run_id).join("events.jsonl")
    }

    fn root_authority(&self) -> Result<RootTransactionAuthority, ShadowError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.transitioning {
            return Err(ShadowError::RootTransition);
        }
        if let Some(authority) = state.root_authority.as_ref() {
            return Ok(authority.clone());
        }
        let authority = RootTransactionAuthority::open(&state.root)?;
        state.root = authority.root().to_path_buf();
        state.root_authority = Some(authority.clone());
        state.generation = state.generation.saturating_add(1);
        Ok(authority)
    }

    fn with_root_read<T>(
        &self,
        operation: impl FnOnce(&Path) -> Result<T, ShadowError>,
    ) -> Result<T, ShadowError> {
        let authority = self.root_authority()?;
        let generation = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .generation;
        let _guard = authority.read_operation();
        self.validate_root_snapshot(&authority, generation)?;
        operation(authority.root())
    }

    fn with_root_write<T>(
        &self,
        operation: impl FnOnce(&Path) -> Result<T, ShadowError>,
    ) -> Result<T, ShadowError> {
        let authority = self.root_authority()?;
        let generation = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .generation;
        let _guard = authority.write_operation();
        self.validate_root_snapshot(&authority, generation)?;
        operation(authority.root())
    }

    fn validate_root_snapshot(
        &self,
        authority: &RootTransactionAuthority,
        generation: u64,
    ) -> Result<(), ShadowError> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let current = !state.transitioning
            && state.generation == generation
            && state
                .root_authority
                .as_ref()
                .is_some_and(|candidate| candidate.same_authority(authority))
            && state.root == authority.root();
        if current {
            Ok(())
        } else {
            Err(ShadowError::RootTransition)
        }
    }

    fn sync_root(&self, root: &Path) -> Result<(), std::io::Error> {
        #[cfg(test)]
        {
            let remaining = self
                .fail_root_sync_on_call
                .load(std::sync::atomic::Ordering::SeqCst);
            if remaining > 0 {
                let previous = self
                    .fail_root_sync_on_call
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                if previous == 1 {
                    return Err(std::io::Error::other("injected root sync failure"));
                }
            }
        }
        sync_directory(root)
    }

    fn authority(
        &self,
        run_id: &str,
        create: bool,
    ) -> Result<Option<Arc<RunAuthority>>, ShadowError> {
        for _attempt in 0..2 {
            let root_authority = self.root_authority()?;
            let _root_guard = root_authority.read_operation();
            let (generation, access) = {
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                let same_root = state
                    .root_authority
                    .as_ref()
                    .is_some_and(|current| current.same_authority(&root_authority))
                    && state.root == root_authority.root();
                if state.transitioning || !same_root {
                    continue;
                }
                state.access_clock = state.access_clock.saturating_add(1);
                let access = state.access_clock;
                if let Some(cached) = state.authorities.get_mut(run_id)
                    && cached.authority.is_open()
                {
                    cached.last_used = access;
                    return Ok(Some(Arc::clone(&cached.authority)));
                }
                state.authorities.remove(run_id);
                (state.generation, access)
            };
            let event_path = root_authority.root().join(run_id).join("events.jsonl");
            if !create && !event_path.exists() {
                return Ok(None);
            }
            let checkpoint_path = root_authority.root().join(run_id).join("checkpoint.json");
            let authority = RunAuthority::open(&event_path, &checkpoint_path, run_id)?;
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let same_generation = !state.transitioning
                && state.generation == generation
                && state
                    .root_authority
                    .as_ref()
                    .is_some_and(|current| current.same_authority(&root_authority))
                && state.root == root_authority.root();
            if !same_generation {
                continue;
            }
            if let Some(cached) = state.authorities.get_mut(run_id)
                && cached.authority.is_open()
            {
                cached.last_used = access;
                return Ok(Some(Arc::clone(&cached.authority)));
            }
            state.authorities.insert(
                run_id.to_string(),
                CachedRunAuthority {
                    authority: Arc::clone(&authority),
                    last_used: access,
                },
            );
            evict_idle_authorities(&mut state, Some(run_id));
            return Ok(Some(authority));
        }
        Err(ShadowError::RootTransition)
    }

    fn clear_cached_authority(&self, run_id: &str, authority: &Arc<RunAuthority>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state
            .authorities
            .get(run_id)
            .is_some_and(|cached| Arc::ptr_eq(&cached.authority, authority))
        {
            state.authorities.remove(run_id);
        }
    }
}

fn cleanup_transactions(root: &Path) -> Result<(), ShadowError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(ShadowError::Io(error.to_string())),
    };
    let mut removed = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                return Err(ShadowError::Io(error.to_string()));
            }
        };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let is_hidden_transaction =
            name.starts_with(".preparing-") || name.starts_with(".deleting-");
        if is_hidden_transaction && entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            std::fs::remove_dir_all(entry.path())
                .map_err(|error| ShadowError::Io(error.to_string()))?;
            removed = true;
        }
    }
    if removed {
        sync_directory(root).map_err(|error| ShadowError::Io(error.to_string()))?;
    }
    Ok(())
}

fn evict_idle_authorities(state: &mut ShadowGenerationState, protected: Option<&str>) {
    while state.authorities.len() > MAX_CACHED_RUN_AUTHORITIES {
        let candidate = state
            .authorities
            .iter()
            .filter(|(run_id, cached)| {
                protected != Some(run_id.as_str()) && cached.authority.cache_evictable()
            })
            .min_by_key(|(_, cached)| cached.last_used)
            .map(|(run_id, _)| run_id.clone());
        let Some(candidate) = candidate else {
            break;
        };
        state.authorities.remove(&candidate);
    }
}

impl FileTaskShadow {
    /// Publish a complete first TaskRun generation with one visible rename.
    pub(crate) fn publish_initial_event_batch(
        &self,
        run_id: &str,
        events: Vec<RuntimeJournalEvent>,
    ) -> Result<(), ShadowError> {
        if events.is_empty()
            || events.first().map(RuntimeJournalEvent::event_type)
                != Some(RuntimeEventKind::RunCreated)
            || !events
                .iter()
                .any(|event| event.event_type() == RuntimeEventKind::PlanRevisionCommitted)
        {
            return Err(ShadowError::Encode(
                "initial task publication requires RunCreated and PlanRevisionCommitted"
                    .to_string(),
            ));
        }
        if events.iter().any(|event| event.run_id() != run_id) {
            return Err(ShadowError::Encode(
                "initial task publication contains an event for another run".to_string(),
            ));
        }

        self.with_root_write(|root| {
        echo_agent::utils::fs::create_dir_all_durable(root)
            .map_err(|error| ShadowError::Io(error.to_string()))?;
        let final_directory = root.join(run_id);
        if final_directory.exists() {
            return Err(ShadowError::Io(format!(
                "task run already exists: {run_id}"
            )));
        }
        let staging_directory = root.join(format!(".preparing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&staging_directory)
            .map_err(|error| ShadowError::Io(error.to_string()))?;

        #[cfg(test)]
        let simulate_crash = self
            .fail_initial_publish_before_rename
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let simulate_crash = false;

        let mut committed = Vec::with_capacity(events.len());
        let stage_result = (|| -> Result<Option<ShadowError>, ShadowError> {
            let authority = RunAuthority::open(
                &staging_directory.join("events.jsonl"),
                &staging_directory.join("checkpoint.json"),
                run_id,
            )?;
            let batch = authority.append_batch(events)?;
            #[cfg(test)]
            let batch = {
                let mut batch = batch;
                if self
                    .fail_initial_batch_durability
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    batch.apply.journal =
                        echo_agent::state::journal::JournalDurabilityStatus::Degraded {
                            error: "injected persistent initial journal durability failure"
                                .to_string(),
                        };
                }
                batch
            };
            if let echo_agent::state::journal::JournalDurabilityStatus::Confirmed =
                &batch.apply.journal
            {
                committed = batch.events;
            } else {
                let detail = match &batch.apply.journal {
                    echo_agent::state::journal::JournalDurabilityStatus::Unconfirmed => {
                        "journal durability remains unconfirmed".to_string()
                    }
                    echo_agent::state::journal::JournalDurabilityStatus::Degraded { error } => {
                        error.clone()
                    }
                    echo_agent::state::journal::JournalDurabilityStatus::Confirmed => {
                        "journal durability changed during staging validation".to_string()
                    }
                };
                return Err(ShadowError::InitialBatchDurabilityDegraded {
                    run_id: run_id.to_string(),
                    detail,
                });
            }
            authority.refresh_projections(false)?;
            sync_directory(&staging_directory)
                .map_err(|error| ShadowError::Io(error.to_string()))?;
            drop(authority);
            if simulate_crash {
                return Err(ShadowError::Io(
                    "injected crash before initial run publication".to_string(),
                ));
            }
            std::fs::rename(&staging_directory, &final_directory)
                .map_err(|error| ShadowError::Io(error.to_string()))?;
            let degradation = self.sync_root(root).err().map(|error| {
                ShadowError::CommittedPublicationDegraded {
                    run_id: run_id.to_string(),
                    detail: error.to_string(),
                }
            });
            Ok(degradation)
        })();
        let degradation = match stage_result {
            Ok(degradation) => degradation,
            Err(error) => {
                if !simulate_crash
                    && let Err(cleanup) = std::fs::remove_dir_all(&staging_directory)
                {
                    tracing::warn!(%cleanup, path = %staging_directory.display(), "failed to remove aborted task publication");
                }
                return Err(error);
            }
        };
        if let Some(hook) = self.event_hook.get() {
            for event in &committed {
                hook(event);
            }
        }
        degradation.map_or(Ok(()), Err)
        })
    }

    #[cfg(test)]
    pub(crate) fn fail_next_initial_publish_before_rename(&self) {
        self.fail_initial_publish_before_rename
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    fn fail_next_initial_batch_durability_for_test(&self) {
        self.fail_initial_batch_durability
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    fn pause_next_deletion_for_test(&self) -> TestPause {
        let pause = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        *self
            .deletion_pause
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Arc::clone(&pause));
        pause
    }

    #[cfg(test)]
    fn pause_next_rebind_for_test(&self) -> TestPause {
        let pause = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        *self
            .rebind_pause
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Arc::clone(&pause));
        pause
    }

    #[cfg(test)]
    pub(crate) fn fail_root_sync_on_call_for_test(&self, call: usize) {
        self.fail_root_sync_on_call
            .store(call, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_append_durability_for_test(
        &self,
        run_id: &str,
    ) -> Result<(), ShadowError> {
        let authority = self
            .authority(run_id, false)?
            .ok_or_else(|| ShadowError::Io(format!("TaskRuntime run not found: {run_id}")))?;
        authority.fail_next_durability_settlement_for_test();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn reconcile_next_append_unconfirmed_for_test(
        &self,
        run_id: &str,
    ) -> Result<(), ShadowError> {
        let authority = self
            .authority(run_id, false)?
            .ok_or_else(|| ShadowError::Io(format!("TaskRuntime run not found: {run_id}")))?;
        authority.reconcile_next_append_unconfirmed_for_test();
        Ok(())
    }

    pub(crate) fn settle_event_state(
        &self,
        run_id: &str,
    ) -> Result<
        (
            echo_agent::state::journal::JournalDurabilityStatus,
            super::history_projection::HistoryProjectionApplyStatus,
        ),
        ShadowError,
    > {
        self.authority(run_id, false)?
            .map(|authority| authority.settle_durability_and_history())
            .ok_or_else(|| ShadowError::Io(format!("TaskRuntime run not found: {run_id}")))
    }

    #[cfg(test)]
    pub(crate) fn fail_next_durability_probe_for_test(
        &self,
        run_id: &str,
    ) -> Result<(), ShadowError> {
        let authority = self
            .authority(run_id, false)?
            .ok_or_else(|| ShadowError::Io(format!("TaskRuntime run not found: {run_id}")))?;
        authority.fail_next_durability_probe_for_test();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_review_history_append_for_test(
        &self,
        run_id: &str,
    ) -> Result<(), ShadowError> {
        let authority = self
            .authority(run_id, false)?
            .ok_or_else(|| ShadowError::Io(format!("TaskRuntime run not found: {run_id}")))?;
        authority.fail_next_review_history_append_for_test();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_history_cursor_writes_for_test(
        &self,
        run_id: &str,
        count: usize,
    ) -> Result<(), ShadowError> {
        let authority = self
            .authority(run_id, false)?
            .ok_or_else(|| ShadowError::Io(format!("TaskRuntime run not found: {run_id}")))?;
        authority.fail_history_cursor_writes_for_test(count);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn history_paths_for_test(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<(PathBuf, PathBuf, PathBuf), ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.history_paths_for_test(task_id))
            .ok_or_else(|| ShadowError::Io(format!("TaskRuntime run not found: {run_id}")))
    }

    #[cfg(test)]
    pub(crate) fn history_stats_for_test(&self, run_id: &str) -> Result<(usize, u64), ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.history_stats_for_test())
            .ok_or_else(|| ShadowError::Io(format!("TaskRuntime run not found: {run_id}")))
    }

    #[cfg(test)]
    pub(crate) fn history_fallback_replay_count_for_test(
        &self,
        run_id: &str,
    ) -> Result<usize, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.history_fallback_replay_count_for_test())
            .ok_or_else(|| ShadowError::Io(format!("TaskRuntime run not found: {run_id}")))
    }

    #[cfg(test)]
    fn cached_authority_count_for_test(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .authorities
            .len()
    }

    #[cfg(test)]
    fn has_cached_authority_for_test(&self, run_id: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .authorities
            .contains_key(run_id)
    }

    #[cfg(test)]
    pub(crate) fn append_event_line(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        step_id: Option<&str>,
        event_type: RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Result<Arc<RuntimeTaskEvent>, ShadowError> {
        self.append_event_line_with_receipt(run_id, task_id, step_id, event_type, payload)
            .map(|(event, _, _)| event)
    }

    pub(crate) fn append_event_line_with_receipt(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        step_id: Option<&str>,
        event_type: RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Result<
        (
            Arc<RuntimeTaskEvent>,
            echo_agent::state::journal::ApplyReceipt,
            super::history_projection::HistoryProjectionApplyStatus,
        ),
        ShadowError,
    > {
        let authority = self
            .authority(run_id, true)?
            .ok_or_else(|| ShadowError::Io("TaskRuntime authority unavailable".to_string()))?;
        let event = RuntimeJournalEvent::for_append(run_id, task_id, step_id, event_type, payload);
        let hook = self.event_hook.get().cloned();
        match authority.append_with_observer(event, |persisted| {
            if let Some(hook) = hook.as_ref() {
                hook(persisted);
            }
        }) {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                self.clear_cached_authority(run_id, &authority);
                Err(error)
            }
        }
    }

    pub(crate) fn append_event_batch(
        &self,
        run_id: &str,
        events: Vec<RuntimeJournalEvent>,
    ) -> Result<RunBatchAppendReceipt, ShadowError> {
        if events.is_empty() {
            return Err(ShadowError::Encode(
                "TaskRuntime batch must contain at least one event".to_string(),
            ));
        }
        if events.iter().any(|event| event.run_id() != run_id) {
            return Err(ShadowError::Encode(
                "TaskRuntime batch contains an event for another run".to_string(),
            ));
        }
        let authority = self
            .authority(run_id, true)?
            .ok_or_else(|| ShadowError::Io("TaskRuntime authority unavailable".to_string()))?;
        let hook = self.event_hook.get().cloned();
        match authority.append_batch_with_observer(events, |persisted| {
            if let Some(hook) = hook.as_ref() {
                hook(persisted);
            }
        }) {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                self.clear_cached_authority(run_id, &authority);
                Err(error)
            }
        }
    }

    pub(crate) fn rewrite_plan(&self, run_id: &str) -> Result<(), ShadowError> {
        if self.root().is_file() {
            return Ok(());
        }
        if let Some(authority) = self.authority(run_id, false)? {
            authority.refresh_projections(false)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn rewrite_plan_with_stats(
        &self,
        run_id: &str,
    ) -> Result<ProjectionRefreshStats, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.refresh_projections(false))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub(crate) fn ensure_projections_current(&self, run_id: &str) -> Result<(), ShadowError> {
        // A generation owner may temporarily make the root unavailable while
        // retaining a terminal-settlement debt. Preserve the previous read
        // contract: absence is reported by the projection read, while the
        // cached authority remains available for a later explicit retry.
        if self.root().is_file() {
            return Ok(());
        }
        if let Some(authority) = self.authority(run_id, false)? {
            authority.refresh_projections(true)?;
        }
        Ok(())
    }

    pub(crate) fn list_run_ids(&self) -> Result<Vec<String>, ShadowError> {
        self.with_root_read(|root| {
            let entries = match std::fs::read_dir(root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(ShadowError::Io(error.to_string())),
            };
            let mut ids = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|error| ShadowError::Io(error.to_string()))?;
                let path = entry.path();
                let hidden = entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with(".preparing-") || name.starts_with(".deleting-")
                });
                if hidden || !path.is_dir() {
                    continue;
                }
                if !path.join("events.jsonl").is_file() {
                    continue;
                }
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    ids.push(name.to_string());
                }
            }
            Ok(ids)
        })
    }

    pub(crate) fn remove_runs(&self, run_ids: &[String]) -> Result<(), ShadowError> {
        let mut run_ids = run_ids.to_vec();
        run_ids.sort();
        run_ids.dedup();
        if run_ids.is_empty() {
            return Ok(());
        }
        for run_id in &run_ids {
            let mut components = Path::new(run_id).components();
            let valid = matches!(components.next(), Some(std::path::Component::Normal(_)))
                && components.next().is_none();
            if run_id.trim().is_empty() || !valid {
                return Err(ShadowError::Io(format!(
                    "refusing to remove invalid task run id: {run_id}"
                )));
            }
        }

        self.with_root_write(|root| {
            echo_agent::utils::fs::create_dir_all_durable(root)
                .map_err(|error| ShadowError::Io(error.to_string()))?;
            let mut invalidations = Vec::new();
            for run_id in &run_ids {
                let event_path = root.join(run_id).join("events.jsonl");
                if event_path.exists() {
                    invalidations.push(RunAuthority::begin_invalidate(&event_path)?);
                }
            }
            #[cfg(test)]
            if let Some(pause) = self
                .deletion_pause
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                pause.0.wait();
                pause.1.wait();
            }
            let tombstone = root.join(format!(".deleting-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&tombstone).map_err(|error| ShadowError::Io(error.to_string()))?;
            if let Err(error) = self.sync_root(root) {
                let _cleanup = std::fs::remove_dir(&tombstone);
                return Err(ShadowError::Io(error.to_string()));
            }
            let mut moved = Vec::<(PathBuf, PathBuf)>::new();
            for run_id in &run_ids {
                let source = root.join(run_id);
                if !source.exists() {
                    continue;
                }
                let target = tombstone.join(run_id);
                if let Err(error) = std::fs::rename(&source, &target) {
                    let mut rollback_errors = Vec::new();
                    for (original, staged) in moved.iter().rev() {
                        if let Err(rollback) = std::fs::rename(staged, original) {
                            rollback_errors.push(rollback.to_string());
                        }
                    }
                    if let Err(cleanup) = std::fs::remove_dir(&tombstone) {
                        rollback_errors.push(cleanup.to_string());
                    }
                    return Err(ShadowError::Io(format!(
                        "failed to stage run {run_id} for deletion: {error}; rollback errors: {}",
                        rollback_errors.join("; ")
                    )));
                }
                moved.push((source, target));
            }
            if !moved.is_empty() {
                sync_directory(&tombstone).map_err(|error| {
                    ShadowError::CommittedDeletionDegraded {
                        tombstone: tombstone.display().to_string(),
                        detail: error.to_string(),
                    }
                })?;
                self.sync_root(root)
                    .map_err(|error| ShadowError::CommittedDeletionDegraded {
                        tombstone: tombstone.display().to_string(),
                        detail: error.to_string(),
                    })?;
            }
            {
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                for run_id in &run_ids {
                    state.authorities.remove(run_id);
                }
            }
            std::fs::remove_dir_all(&tombstone).map_err(|error| {
                ShadowError::CommittedDeletionDegraded {
                    tombstone: tombstone.display().to_string(),
                    detail: error.to_string(),
                }
            })?;
            self.sync_root(root)
                .map_err(|error| ShadowError::CommittedDeletionDegraded {
                    tombstone: tombstone.display().to_string(),
                    detail: error.to_string(),
                })?;
            drop(invalidations);
            Ok(())
        })
    }

    pub(crate) fn read_plan(&self, run_id: &str) -> Result<Option<PlanRevision>, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.read_plan_projection())
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) fn read_run_state(
        &self,
        run_id: &str,
    ) -> Result<Option<RunStateSnapshot>, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.read_run_state_projection())
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) fn read_todo_query_projection(
        &self,
        run_id: &str,
    ) -> Result<Option<super::event_rebuild::TodoQueryProjection>, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.read_todo_query_projection())
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) fn read_completion_gate_projection(
        &self,
        run_id: &str,
    ) -> Result<Option<super::event_rebuild::CompletionGateProjection>, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.read_completion_gate_projection())
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) fn read_artifacts_projection(
        &self,
        run_id: &str,
    ) -> Result<Vec<super::types::Artifact>, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.read_artifacts_projection())
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub(crate) fn read_reviews_projection(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Vec<super::types::ReviewResult>, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.read_reviews_projection(task_id))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub(crate) fn read_summary_projection(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<super::types::TaskExecutionSummary>, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.read_summary_projection(task_id))
            .transpose()
            .map(Option::flatten)
    }

    #[cfg(test)]
    pub(crate) fn read_events(&self, run_id: &str) -> Result<Vec<RuntimeTaskEvent>, ShadowError> {
        self.read_events_after(run_id, 0)
    }

    pub(crate) fn read_events_after(
        &self,
        run_id: &str,
        after_sequence: u64,
    ) -> Result<Vec<RuntimeTaskEvent>, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.replay_after(after_sequence))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub(crate) fn diagnostic_full_replay(
        &self,
        run_id: &str,
    ) -> Result<Option<RunStateSnapshot>, ShadowError> {
        self.authority(run_id, false)?
            .map(|authority| authority.diagnostic_full_replay())
            .transpose()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProjectionRefreshStats {
    pub(crate) used_checkpoint: bool,
    pub(crate) folded_events: usize,
    pub(crate) seq: i64,
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ShadowError {
    #[error("shadow I/O: {0}")]
    Io(String),
    #[error("shadow encode: {0}")]
    Encode(String),
    #[error("shadow decode: {0}")]
    Decode(String),
    #[error("shadow rebuild: {0}")]
    Rebuild(String),
    #[error("event {seq} committed but projection refresh degraded: {detail}")]
    CommittedProjectionDegraded { seq: i64, detail: String },
    #[error("TaskRun {run_id} is visible but root publication durability degraded: {detail}")]
    CommittedPublicationDegraded { run_id: String, detail: String },
    #[error(
        "TaskRun {run_id} initial journal batch is hidden because durability degraded: {detail}"
    )]
    InitialBatchDurabilityDegraded { run_id: String, detail: String },
    #[error("TaskRun deletion is staged at {tombstone} but durability degraded: {detail}")]
    CommittedDeletionDegraded { tombstone: String, detail: String },
    #[error("TaskRuntime root generation is transitioning")]
    RootTransition,
    #[error("TaskRuntime authority is closed: {0}")]
    AuthorityClosed(String),
    #[error("next journal sequence {next_sequence} exceeds the EKO i64 cursor domain")]
    SequenceCapacityExceeded { next_sequence: u64 },
    #[error("journal sequence {sequence} exceeds the EKO i64 cursor domain")]
    SequenceOutOfRange { sequence: u64 },
    #[error("TaskRuntime batch {batch_id} was not committed after {attempts} attempts: {detail}")]
    BatchNotCommitted {
        batch_id: String,
        attempts: usize,
        detail: String,
    },
    #[error(
        "TaskRuntime batch {batch_id} ({payload_digest}) has an unknown outcome after verified reopen: {detail}"
    )]
    BatchOutcomeUnknown {
        batch_id: String,
        payload_digest: String,
        detail: String,
    },
    #[error(
        "TaskRuntime batch {batch_id} ({payload_digest}) conflicts with journal authority: {detail}"
    )]
    BatchIdentityConflict {
        batch_id: String,
        payload_digest: String,
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::store::TaskRuntimeStore;
    use crate::tasks::task_runtime::types::{
        AttendedMode, DomainProfile, ExecutionMode, PlanRevision, PlanTask, PlanTaskKind,
        TaskPatch, TaskPlan, TaskRunStatus, TaskUpdateOperation, TaskUpdateRequest,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn task(id: &str) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            title: format!("task {id}"),
            description: format!("do {id}"),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            domain_profile: DomainProfile::General,
            depends_on: Vec::new(),
            parallel_group: None,
            execution_target: None,
            files: Vec::new(),
            allowed_tools: vec!["read_file".to_string()],
            required_artifacts: Vec::new(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: echo_agent::tasks::TaskStatus::Pending,
            claim: None,
            sort_order: 0,
        }
    }

    fn run_created(
        shadow: &FileTaskShadow,
        run_id: &str,
    ) -> Result<Arc<RuntimeTaskEvent>, ShadowError> {
        shadow.append_event_line(
            run_id,
            None,
            None,
            RuntimeEventKind::RunCreated,
            serde_json::json!({
                "goal": "journal authority",
                "goal_revision": 1,
                "goal_sha256": crate::tasks::task_runtime::task_goal_sha256("journal authority"),
                "domain_profile": "general",
                "workspace_id": "workspace",
                "conversation_id": "conversation",
                "root_message_id": "root",
                "route": "complex",
                "attended_mode": "attended",
                "created_at": echo_agent::utils::time::to_local(chrono::Utc::now()).to_rfc3339(),
            }),
        )
    }

    #[test]
    fn checkpoint_warm_suffix_preserves_usage_and_compaction_deduplication() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "dedupe").map_err(|error| error.to_string())?;
        for (kind, payload) in [
            (
                RuntimeEventKind::RunTurnStarted,
                serde_json::json!({
                    "turn_id": "turn-1", "ordinal": 0, "origin": "continuation",
                    "transcript_visibility": "internal"
                }),
            ),
            (
                RuntimeEventKind::RunTurnUsageAccounted,
                serde_json::json!({
                    "event_id": "usage-1", "turn_id": "turn-1",
                    "input_tokens": 2, "output_tokens": 3, "elapsed_seconds": 0
                }),
            ),
            (
                RuntimeEventKind::RunTurnCompacted,
                serde_json::json!({"event_id": "compact-1", "turn_id": "turn-1"}),
            ),
        ] {
            shadow
                .append_event_line("dedupe", None, None, kind, payload)
                .map_err(|error| error.to_string())?;
        }
        let cold = shadow
            .rewrite_plan_with_stats("dedupe")
            .map_err(|error| error.to_string())?;
        assert!(!cold.used_checkpoint);
        assert_eq!(cold.folded_events, 4);
        for (kind, payload) in [
            (
                RuntimeEventKind::RunTurnUsageAccounted,
                serde_json::json!({
                    "event_id": "usage-1", "turn_id": "turn-1",
                    "input_tokens": 2, "output_tokens": 3, "elapsed_seconds": 0
                }),
            ),
            (
                RuntimeEventKind::RunTurnCompacted,
                serde_json::json!({"event_id": "compact-1", "turn_id": "turn-1"}),
            ),
        ] {
            shadow
                .append_event_line("dedupe", None, None, kind, payload)
                .map_err(|error| error.to_string())?;
        }
        let warm = shadow
            .rewrite_plan_with_stats("dedupe")
            .map_err(|error| error.to_string())?;
        assert!(warm.used_checkpoint);
        assert_eq!(warm.folded_events, 2);
        let continuation = shadow
            .read_run_state("dedupe")
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "continuation projection missing".to_string())?;
        assert_eq!(continuation.tokens_used, 5);
        assert_eq!(continuation.compaction_count, 1);
        Ok(())
    }

    #[test]
    fn corrupt_ahead_and_behind_checkpoints_recover_from_journal() -> Result<(), String> {
        for case in ["corrupt", "tampered", "ahead", "behind"] {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let run_id = format!("checkpoint-{case}");
            let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
            run_created(&shadow, &run_id).map_err(|error| error.to_string())?;
            shadow
                .rewrite_plan(&run_id)
                .map_err(|error| error.to_string())?;
            shadow
                .append_event_line(
                    &run_id,
                    None,
                    None,
                    RuntimeEventKind::RunStatusChanged,
                    serde_json::json!({"from": "pending", "to": "running"}),
                )
                .map_err(|error| error.to_string())?;
            if case != "behind" {
                shadow
                    .rewrite_plan(&run_id)
                    .map_err(|error| error.to_string())?;
            }
            let checkpoint_path = temp.path().join(&run_id).join("checkpoint.json");
            drop(shadow);
            match case {
                "corrupt" => std::fs::write(&checkpoint_path, b"{corrupt")
                    .map_err(|error| error.to_string())?,
                "tampered" => {
                    let mut value: serde_json::Value = serde_json::from_slice(
                        &std::fs::read(&checkpoint_path).map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                    value["state"]["run"] = serde_json::Value::Null;
                    std::fs::write(
                        &checkpoint_path,
                        serde_json::to_vec(&value).map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                }
                "ahead" => {
                    use echo_agent::state::journal::{CheckpointStore, FileCheckpointStore};
                    let store = FileCheckpointStore::<serde_json::Value>::open(&checkpoint_path);
                    let frame = store
                        .load()
                        .map_err(|error| error.to_string())?
                        .ok_or_else(|| "checkpoint missing".to_string())?;
                    store
                        .save(&frame.state, 999)
                        .map_err(|error| error.to_string())?;
                }
                "behind" => {}
                _ => return Err("unknown checkpoint case".to_string()),
            }
            let reopened = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
            let stats = reopened
                .rewrite_plan_with_stats(&run_id)
                .map_err(|error| error.to_string())?;
            assert_eq!(stats.folded_events, if case == "behind" { 1 } else { 2 });
            assert_eq!(stats.used_checkpoint, case == "behind");
            assert_eq!(
                reopened
                    .read_run_state(&run_id)
                    .map_err(|error| error.to_string())?
                    .map(|state| state.run.status),
                Some(TaskRunStatus::Running)
            );
        }
        Ok(())
    }

    #[test]
    fn committed_projection_failure_self_heals_without_duplicate_replay() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "degraded").map_err(|error| error.to_string())?;
        shadow
            .rewrite_plan("degraded")
            .map_err(|error| error.to_string())?;
        let checkpoint = temp.path().join("degraded/checkpoint.json");
        std::fs::remove_file(&checkpoint).map_err(|error| error.to_string())?;
        std::fs::create_dir(&checkpoint).map_err(|error| error.to_string())?;
        let committed = shadow
            .append_event_line(
                "degraded",
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({"from": "pending", "to": "running"}),
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            shadow.rewrite_plan("degraded"),
            Err(ShadowError::CommittedProjectionDegraded { seq, .. }) if seq == committed.seq
        ));
        assert_eq!(
            shadow
                .read_events("degraded")
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::RunStatusChanged)
                .count(),
            1
        );
        std::fs::remove_dir(&checkpoint).map_err(|error| error.to_string())?;
        shadow
            .rewrite_plan("degraded")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            shadow
                .read_run_state("degraded")
                .map_err(|error| error.to_string())?
                .map(|state| state.run.status),
            Some(TaskRunStatus::Running)
        );
        Ok(())
    }

    #[test]
    fn incremental_sequence_rewinds_plan_from_the_reducer_projection() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let first = run_created(&shadow, "plan").map_err(|error| error.to_string())?;
        let second = shadow
            .append_event_line(
                "plan",
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                serde_json::json!({
                    "reason": "initial",
                    "base_revision": 0,
                    "skipped_task_ids": [],
                    "plan": PlanRevision {
                        plan_id: "plan-1".to_string(),
                        run_id: "plan".to_string(),
                        revision: 1,
                        domain_profile: DomainProfile::General,
                        goal_revision: 1,
                        goal_sha256: crate::tasks::task_runtime::task_goal_sha256("journal authority"),
                        assumptions: Vec::new(),
                        risks: Vec::new(),
                        execution_mode: ExecutionMode::Parallel,
                        tasks: vec![task("task-1").spec()],
                    }
                }),
            )
            .map_err(|error| error.to_string())?;
        let third = shadow
            .append_event_line(
                "plan",
                Some("task-1"),
                None,
                RuntimeEventKind::TaskStarted,
                serde_json::json!({"status": "running", "owner_agent": "explorer"}),
            )
            .map_err(|error| error.to_string())?;
        assert_eq!([first.seq, second.seq, third.seq], [1, 2, 3]);
        shadow
            .rewrite_plan("plan")
            .map_err(|error| error.to_string())?;
        let plan = shadow
            .read_plan("plan")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "plan projection missing".to_string())?;
        let state = shadow
            .read_run_state("plan")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run-state projection missing".to_string())?;
        assert_eq!(plan.plan_id, "plan-1");
        assert_eq!(
            state.tasks.first().map(|task| task.status.clone()),
            Some(echo_agent::tasks::TaskStatus::Running)
        );
        Ok(())
    }

    #[test]
    fn lifecycle_reorder_skip_status_and_multiple_runs_keep_projection_parity() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path())
            .map_err(|error| error.to_string())?;
        let reader = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        for run_id in ["first", "second"] {
            let goal = format!("goal {run_id}");
            store
                .create_run(
                    run_id,
                    "workspace",
                    run_id,
                    "message",
                    DomainProfile::General,
                    &goal,
                    "complex",
                    AttendedMode::Attended,
                )
                .map_err(|error| error.to_string())?;
            store
                .attach_plan_for_test(&TaskPlan {
                    plan_id: format!("plan-{run_id}"),
                    run_id: run_id.to_string(),
                    revision: 1,
                    domain_profile: DomainProfile::General,
                    goal_revision: 1,
                    goal_sha256: crate::tasks::task_runtime::task_goal_sha256(&goal),
                    assumptions: Vec::new(),
                    risks: Vec::new(),
                    execution_mode: ExecutionMode::Parallel,
                    tasks: vec![task(&format!("{run_id}-a")), task(&format!("{run_id}-b"))],
                })
                .map_err(|error| error.to_string())?;
        }
        store
            .apply_task_patch_for_test(
                "first",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "reorder then skip".to_string(),
                    operations: vec![
                        TaskUpdateOperation::Reorder {
                            task_ids: vec!["first-b".to_string(), "first-a".to_string()],
                        },
                        TaskUpdateOperation::Update {
                            task_id: "first-a".to_string(),
                            patch: TaskPatch {
                                title: Some("renamed first task".to_string()),
                                ..Default::default()
                            },
                        },
                    ],
                },
            )
            .map_err(|error| error.to_string())?;
        store
            .apply_task_patch_for_test(
                "first",
                &TaskUpdateRequest {
                    base_revision: 2,
                    reason: "no longer required".to_string(),
                    operations: vec![TaskUpdateOperation::Skip {
                        task_id: "first-b".to_string(),
                    }],
                },
            )
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "first",
                "first-a",
                echo_agent::tasks::TaskStatus::Running,
                Some("explorer"),
                None,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("first", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("first", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        for run_id in ["first", "second"] {
            let public_events = store
                .list_events(run_id, 0)
                .map_err(|error| error.to_string())?;
            let authority_events = reader
                .read_events(run_id)
                .map_err(|error| error.to_string())?;
            assert_eq!(
                serde_json::to_value(public_events).map_err(|error| error.to_string())?,
                serde_json::to_value(authority_events).map_err(|error| error.to_string())?
            );
        }
        let plan = reader
            .read_plan("first")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "first plan missing".to_string())?;
        assert_eq!(
            plan.tasks.first().map(|task| task.id.as_str()),
            Some("first-b")
        );
        assert_eq!(
            plan.tasks.get(1).map(|task| task.title.as_str()),
            Some("renamed first task")
        );
        let state = reader
            .read_run_state("first")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "first state missing".to_string())?;
        assert_eq!(state.run.status, TaskRunStatus::Paused);
        assert_eq!(
            state
                .tasks
                .iter()
                .find(|task| task.task_id == "first-b")
                .map(|task| task.status.clone()),
            Some(echo_agent::tasks::TaskStatus::Skipped)
        );
        assert!(
            reader
                .read_plan("second")
                .map_err(|error| error.to_string())?
                .is_some_and(|plan| plan.tasks.iter().all(|task| task.id.starts_with("second-")))
        );
        Ok(())
    }

    #[test]
    fn same_run_projection_waits_for_authority_append_boundary() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "locked").map_err(|error| error.to_string())?;
        let authority = shadow
            .authority("locked", false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "authority missing".to_string())?;
        let guard = authority.lock_operation_for_test();
        let writer = shadow.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = writer
                .rewrite_plan("locked")
                .map_err(|error| error.to_string());
            let _delivered = sender.send(result);
        });
        assert!(matches!(
            receiver.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(guard);
        receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| error.to_string())??;
        handle
            .join()
            .map_err(|_| "projection thread panicked".to_string())?;
        Ok(())
    }

    #[test]
    #[ignore = "TaskRuntime journal performance characterization; run explicitly"]
    fn benchmark_1k_turns_10k_events_100_compactions() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let run_id = "benchmark";
        run_created(&shadow, run_id).map_err(|error| error.to_string())?;
        shadow
            .append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::RunContinuationConfigured,
                serde_json::json!({"enabled": true}),
            )
            .map_err(|error| error.to_string())?;
        for ordinal in 0_u64..1_000 {
            let turn_id = format!("turn-{ordinal}");
            for (kind, payload) in [
                (
                    RuntimeEventKind::RunTurnStarted,
                    serde_json::json!({
                        "turn_id": turn_id, "ordinal": ordinal, "origin": "continuation",
                        "transcript_visibility": "internal"
                    }),
                ),
                (
                    RuntimeEventKind::RunTurnUsageAccounted,
                    serde_json::json!({
                        "event_id": format!("usage-{ordinal}"), "turn_id": turn_id,
                        "input_tokens": 1, "output_tokens": 1, "elapsed_seconds": 0
                    }),
                ),
                (
                    RuntimeEventKind::RunTurnFinished,
                    serde_json::json!({
                        "turn_id": turn_id, "status": "ended", "elapsed_seconds": 0,
                        "made_progress": true
                    }),
                ),
            ] {
                shadow
                    .append_event_line(run_id, None, None, kind, payload)
                    .map_err(|error| error.to_string())?;
            }
            if ordinal < 100 {
                shadow
                    .append_event_line(
                        run_id,
                        None,
                        None,
                        RuntimeEventKind::RunTurnCompacted,
                        serde_json::json!({
                            "event_id": format!("compact-{ordinal}"), "turn_id": turn_id
                        }),
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
        let mut count = 3_102_usize;
        while count < 10_000 {
            shadow
                .append_event_line(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"kind": "benchmark", "ordinal": count}),
                )
                .map_err(|error| error.to_string())?;
            count = count.saturating_add(1);
        }
        let started = std::time::Instant::now();
        shadow
            .rewrite_plan(run_id)
            .map_err(|error| error.to_string())?;
        let elapsed = started.elapsed();
        let events = shadow
            .read_events(run_id)
            .map_err(|error| error.to_string())?;
        let continuation = shadow
            .read_run_state(run_id)
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "benchmark continuation missing".to_string())?;
        assert_eq!(events.len(), 10_000);
        assert_eq!(continuation.tokens_used, 2_000);
        assert_eq!(continuation.compaction_count, 100);
        println!("TaskRuntime 10k projection: {elapsed:?}");
        Ok(())
    }

    #[test]
    fn same_run_concurrent_appends_use_one_framework_sequence_authority() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = Arc::new(FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?);
        run_created(&shadow, "run").map_err(|error| error.to_string())?;
        let mut threads = Vec::new();
        for index in 0..64 {
            let shadow = Arc::clone(&shadow);
            threads.push(std::thread::spawn(move || {
                shadow.append_event_line(
                    "run",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"index": index}),
                )
            }));
        }
        for thread in threads {
            thread
                .join()
                .map_err(|_| "append thread panicked".to_string())?
                .map_err(|error| error.to_string())?;
        }
        let events = shadow
            .read_events("run")
            .map_err(|error| error.to_string())?;
        assert_eq!(events.len(), 65);
        assert_eq!(
            events.iter().map(|event| event.seq).collect::<Vec<_>>(),
            (1_i64..=65).collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn same_path_first_open_converges_while_different_paths_open_in_parallel() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first =
            FileTaskShadow::new(temp.path().join("first")).map_err(|error| error.to_string())?;
        let alias =
            FileTaskShadow::new(temp.path().join("first")).map_err(|error| error.to_string())?;
        let other =
            FileTaskShadow::new(temp.path().join("other")).map_err(|error| error.to_string())?;
        let path = temp.path().join("first/shared/events.jsonl");
        let pause =
            RunAuthority::pause_next_open_for_test(&path).map_err(|error| error.to_string())?;
        let opening = first.clone();
        let first_handle = std::thread::spawn(move || run_created(&opening, "shared"));
        pause.0.wait();
        let other_open = other.clone();
        let (other_tx, other_rx) = std::sync::mpsc::channel();
        let other_handle = std::thread::spawn(move || {
            let result = run_created(&other_open, "independent");
            let _sent = other_tx.send(result);
        });
        assert_eq!(
            other_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?
                .seq,
            1
        );
        let alias_open = alias.clone();
        let (alias_tx, alias_rx) = std::sync::mpsc::channel();
        let alias_handle = std::thread::spawn(move || {
            let result = alias_open.append_event_line(
                "shared",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({"alias": true}),
            );
            let _sent = alias_tx.send(result);
        });
        assert!(matches!(
            alias_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        pause.1.wait();
        let first_sequence = first_handle
            .join()
            .map_err(|_| "first open panicked".to_string())?
            .map_err(|error| error.to_string())?
            .seq;
        let alias_sequence = alias_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?
            .seq;
        let mut sequences = [first_sequence, alias_sequence];
        sequences.sort_unstable();
        assert_eq!(sequences, [1, 2]);
        alias_handle
            .join()
            .map_err(|_| "alias open panicked".to_string())?;
        other_handle
            .join()
            .map_err(|_| "other open panicked".to_string())?;
        Ok(())
    }

    #[test]
    fn lookup_held_slots_survive_amortized_registry_prune() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("root");
        assert!(
            RootTransactionAuthority::held_lookup_survives_prune_for_test(&root)
                .map_err(|error| error.to_string())?
        );
        let run_dir = root.join("run");
        std::fs::create_dir_all(&run_dir).map_err(|error| error.to_string())?;
        assert!(
            RunAuthority::held_lookup_survives_prune_for_test(
                &run_dir.join("events.jsonl"),
                &run_dir.join("checkpoint.json"),
                "run",
            )
            .map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[test]
    fn root_last_handle_closing_blocks_immediate_reopen_until_lease_release() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("closing");
        std::fs::create_dir(&root).map_err(|error| error.to_string())?;
        let authority = RootTransactionAuthority::open(&root).map_err(|error| error.to_string())?;
        let pause = RootTransactionAuthority::pause_next_drop_for_test(&root)
            .map_err(|error| error.to_string())?;
        let drop_handle = std::thread::spawn(move || drop(authority));
        pause.0.wait();
        let open_root = root.clone();
        let (open_tx, open_rx) = std::sync::mpsc::channel();
        let open_handle = std::thread::spawn(move || {
            let result = RootTransactionAuthority::open(&open_root);
            let _sent = open_tx.send(result);
        });
        assert!(matches!(
            open_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        pause.1.wait();
        drop_handle
            .join()
            .map_err(|_| "root drop panicked".to_string())?;
        let reopened = open_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened.root(),
            std::fs::canonicalize(&root).map_err(|error| error.to_string())?
        );
        open_handle
            .join()
            .map_err(|_| "root reopen panicked".to_string())?;
        Ok(())
    }

    #[test]
    fn different_runs_append_in_parallel_and_poll_uses_string_safe_sequence() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = Arc::new(FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?);
        let mut threads = Vec::new();
        for run_id in ["a", "b"] {
            let shadow = Arc::clone(&shadow);
            threads.push(std::thread::spawn(move || -> Result<(), ShadowError> {
                run_created(&shadow, run_id)?;
                for index in 0..16 {
                    shadow.append_event_line(
                        run_id,
                        None,
                        None,
                        RuntimeEventKind::Note,
                        serde_json::json!({"index": index}),
                    )?;
                }
                Ok(())
            }));
        }
        for thread in threads {
            thread
                .join()
                .map_err(|_| "append thread panicked".to_string())?
                .map_err(|error| error.to_string())?;
        }
        for run_id in ["a", "b"] {
            let tail = shadow
                .read_events_after(run_id, 12)
                .map_err(|error| error.to_string())?;
            assert_eq!(tail.first().map(|event| event.seq), Some(13));
            let encoded = serde_json::to_value(&tail).map_err(|error| error.to_string())?;
            assert!(
                encoded
                    .get(0)
                    .and_then(|event| event.get("seq"))
                    .is_some_and(serde_json::Value::is_string)
            );
        }
        Ok(())
    }

    #[test]
    fn hooks_fire_once_after_durable_append() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let hook_calls = Arc::clone(&calls);
        let hook_observed = Arc::clone(&observed);
        assert!(shadow.try_attach_event_hook(Arc::new(move |event| {
            hook_calls.fetch_add(1, Ordering::SeqCst);
            hook_observed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event.seq);
        })));
        run_created(&shadow, "run").map_err(|error| error.to_string())?;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            observed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            &[1]
        );
        assert_eq!(
            shadow
                .read_events("run")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn torn_tail_is_repaired_before_the_next_authoritative_append() -> Result<(), String> {
        use std::io::Write;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "torn").map_err(|error| error.to_string())?;
        let event_path = shadow.events_path("torn");
        drop(shadow);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&event_path)
            .map_err(|error| error.to_string())?;
        file.write_all(b"{\"sequence\":2")
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);

        let reopened = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .read_events("torn")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        let next = reopened
            .append_event_line(
                "torn",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({"after": "repair"}),
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(next.seq, 2);
        Ok(())
    }

    #[test]
    fn mid_file_corruption_and_sequence_gap_fail_closed() -> Result<(), String> {
        for case in ["corrupt", "gap"] {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
            run_created(&shadow, case).map_err(|error| error.to_string())?;
            shadow
                .append_event_line(
                    case,
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"case": case}),
                )
                .map_err(|error| error.to_string())?;
            let path = shadow.events_path(case);
            drop(shadow);
            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
            let mut lines = bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(Vec::from)
                .collect::<Vec<_>>();
            if case == "corrupt" {
                let first = lines
                    .first_mut()
                    .ok_or_else(|| "first journal line missing".to_string())?;
                *first = b"not-json".to_vec();
            } else {
                let second = lines
                    .get_mut(1)
                    .ok_or_else(|| "second journal line missing".to_string())?;
                let mut value: serde_json::Value =
                    serde_json::from_slice(second).map_err(|error| error.to_string())?;
                value["sequence"] = serde_json::json!(3_u64);
                *second = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
            }
            let mut damaged = Vec::new();
            for line in lines {
                damaged.extend_from_slice(&line);
                damaged.push(b'\n');
            }
            std::fs::write(&path, damaged).map_err(|error| error.to_string())?;
            let reopened = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
            assert!(matches!(
                reopened.read_events(case),
                Err(ShadowError::Rebuild(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn independent_shadow_handles_share_the_canonical_run_authority() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let second = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&first, "shared").map_err(|error| error.to_string())?;
        let handles = [first.clone(), second]
            .into_iter()
            .map(|shadow| {
                std::thread::spawn(move || -> Result<Vec<i64>, ShadowError> {
                    let mut sequences = Vec::new();
                    for index in 0..24 {
                        sequences.push(
                            shadow
                                .append_event_line(
                                    "shared",
                                    None,
                                    None,
                                    RuntimeEventKind::Note,
                                    serde_json::json!({"index": index}),
                                )?
                                .seq,
                        );
                    }
                    Ok(sequences)
                })
            })
            .collect::<Vec<_>>();
        let mut sequences = Vec::new();
        for handle in handles {
            sequences.extend(
                handle
                    .join()
                    .map_err(|_| "append thread panicked".to_string())?
                    .map_err(|error| error.to_string())?,
            );
        }
        sequences.sort_unstable();
        assert_eq!(sequences, (2_i64..=49).collect::<Vec<_>>());
        assert_eq!(
            first
                .read_events("shared")
                .map_err(|error| error.to_string())?
                .len(),
            49
        );
        Ok(())
    }

    #[test]
    fn concurrent_hooks_follow_durable_sequence_order() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "hook-order").map_err(|error| error.to_string())?;
        let entered = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let hook_entered = Arc::clone(&entered);
        let hook_observed = Arc::clone(&observed);
        assert!(shadow.try_attach_event_hook(Arc::new(move |event| {
            hook_observed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event.seq);
            if event.seq == 2 {
                hook_entered.0.wait();
                hook_entered.1.wait();
            }
        })));
        let first = shadow.clone();
        let first_handle = std::thread::spawn(move || {
            first.append_event_line(
                "hook-order",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({"ordinal": 1}),
            )
        });
        entered.0.wait();
        let second = shadow.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let second_handle = std::thread::spawn(move || {
            let result = second.append_event_line(
                "hook-order",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({"ordinal": 2}),
            );
            let _sent = done_tx.send(result);
        });
        assert!(matches!(
            done_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        entered.1.wait();
        first_handle
            .join()
            .map_err(|_| "first hook append panicked".to_string())?
            .map_err(|error| error.to_string())?;
        done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        second_handle
            .join()
            .map_err(|_| "second hook append panicked".to_string())?;
        assert_eq!(
            observed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            &[2, 3]
        );
        Ok(())
    }

    #[test]
    fn first_batch_hook_observes_the_complete_physical_frame() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "batch-hook").map_err(|error| error.to_string())?;
        let event_path = temp.path().join("batch-hook/events.jsonl");
        let observed_record_counts = Arc::new(Mutex::new(Vec::new()));
        let hook_counts = Arc::clone(&observed_record_counts);
        assert!(shadow.try_attach_event_hook(Arc::new(move |_event| {
            let count = std::fs::read_to_string(&event_path)
                .ok()
                .and_then(|contents| contents.lines().last().map(str::to_string))
                .and_then(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
                .and_then(|frame| {
                    frame
                        .get("records")
                        .and_then(serde_json::Value::as_array)
                        .map(Vec::len)
                })
                .unwrap_or_default();
            hook_counts
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(count);
        })));
        shadow
            .append_event_batch(
                "batch-hook",
                (0..3)
                    .map(|ordinal| {
                        RuntimeJournalEvent::for_append(
                            "batch-hook",
                            None,
                            None,
                            RuntimeEventKind::Note,
                            serde_json::json!({ "ordinal": ordinal }),
                        )
                    })
                    .collect(),
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(
            observed_record_counts
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            &[3, 3, 3]
        );
        Ok(())
    }

    #[test]
    fn concurrent_batches_never_interleave_events_or_hooks() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "batch-order").map_err(|error| error.to_string())?;
        let observed = Arc::new(Mutex::new(Vec::new()));
        let hook_observed = Arc::clone(&observed);
        assert!(shadow.try_attach_event_hook(Arc::new(move |event| {
            hook_observed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event.seq);
        })));
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for group in ["a", "b"] {
            let writer = shadow.clone();
            let start = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                start.wait();
                writer.append_event_batch(
                    "batch-order",
                    (0..3)
                        .map(|ordinal| {
                            RuntimeJournalEvent::for_append(
                                "batch-order",
                                None,
                                None,
                                RuntimeEventKind::Note,
                                serde_json::json!({ "group": group, "ordinal": ordinal }),
                            )
                        })
                        .collect(),
                )
            }));
        }
        barrier.wait();
        for handle in handles {
            handle
                .join()
                .map_err(|_| "batch append thread panicked".to_string())?
                .map_err(|error| error.to_string())?;
        }
        let groups = shadow
            .read_events("batch-order")
            .map_err(|error| error.to_string())?
            .into_iter()
            .skip(1)
            .filter_map(|event| {
                event
                    .payload
                    .get("group")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();
        assert!(
            groups == ["a", "a", "a", "b", "b", "b"] || groups == ["b", "b", "b", "a", "a", "a"]
        );
        assert_eq!(
            observed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            &[2, 3, 4, 5, 6, 7]
        );
        Ok(())
    }

    #[test]
    fn delete_closes_other_shadow_then_same_id_recreates_at_sequence_one() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let deleting = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let other = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&deleting, "deleted").map_err(|error| error.to_string())?;
        let stale = other
            .authority("deleted", false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stale authority missing".to_string())?;
        let pause = deleting.pause_next_deletion_for_test();
        let deleting_thread = deleting.clone();
        let handle =
            std::thread::spawn(move || deleting_thread.remove_runs(&["deleted".to_string()]));
        pause.0.wait();
        let appending = other.clone();
        let (append_tx, append_rx) = std::sync::mpsc::channel();
        let append_handle = std::thread::spawn(move || {
            let result = appending.append_event_line(
                "deleted",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({"goal": "recreated"}),
            );
            let _sent = append_tx.send(result);
        });
        assert!(matches!(
            append_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        pause.1.wait();
        handle
            .join()
            .map_err(|_| "delete thread panicked".to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            stale.append(RuntimeJournalEvent::for_append(
                "deleted",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({"stale": true}),
            )),
            Err(ShadowError::AuthorityClosed(_))
        ));
        let recreated = append_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        append_handle
            .join()
            .map_err(|_| "append thread panicked".to_string())?;
        assert_eq!(recreated.seq, 1);
        Ok(())
    }

    #[test]
    fn authority_open_serializes_with_rebind_and_stale_handle_closes() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let old_root = temp.path().join("old");
        let new_root = temp.path().join("new");
        let moving = FileTaskShadow::new(&old_root).map_err(|error| error.to_string())?;
        let old_reader = FileTaskShadow::new(&old_root).map_err(|error| error.to_string())?;
        run_created(&moving, "known").map_err(|error| error.to_string())?;
        let stale = old_reader
            .authority("known", false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "old authority missing".to_string())?;
        let pause = moving.pause_next_rebind_for_test();
        let rebinding = moving.clone();
        let new_root_for_thread = new_root.clone();
        let rebind_handle = std::thread::spawn(move || rebinding.rebind_root(new_root_for_thread));
        pause.0.wait();
        let opening = old_reader.clone();
        let (open_tx, open_rx) = std::sync::mpsc::channel();
        let open_handle = std::thread::spawn(move || {
            let result = opening.append_event_line(
                "unseen",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({"goal": "old unseen"}),
            );
            let _sent = open_tx.send(result);
        });
        assert!(matches!(
            open_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        pause.1.wait();
        rebind_handle
            .join()
            .map_err(|_| "rebind thread panicked".to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(
            open_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?
                .seq,
            1
        );
        open_handle
            .join()
            .map_err(|_| "authority open panicked".to_string())?;
        assert!(matches!(
            stale.append(RuntimeJournalEvent::for_append(
                "known",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({"stale": true}),
            )),
            Err(ShadowError::AuthorityClosed(_))
        ));
        assert_eq!(
            old_reader
                .append_event_line(
                    "known",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"old": "reopened"}),
                )
                .map_err(|error| error.to_string())?
                .seq,
            2
        );
        assert_eq!(
            moving
                .append_event_line(
                    "known",
                    None,
                    None,
                    RuntimeEventKind::RunCreated,
                    serde_json::json!({"goal": "new"}),
                )
                .map_err(|error| error.to_string())?
                .seq,
            1
        );
        Ok(())
    }

    #[test]
    fn constructors_do_not_clean_live_root_transactions() -> Result<(), String> {
        for prefix in [".preparing-live", ".deleting-live"] {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let first = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
            let authority = first.root_authority().map_err(|error| error.to_string())?;
            let guard = authority.write_operation();
            let live = temp.path().join(prefix);
            std::fs::create_dir_all(&live).map_err(|error| error.to_string())?;
            let root = temp.path().to_path_buf();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let handle = std::thread::spawn(move || {
                let shadow = FileTaskShadow::new(root);
                let _sent = done_tx.send(shadow);
            });
            assert!(matches!(
                done_rx.recv_timeout(std::time::Duration::from_millis(50)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            assert!(live.exists());
            std::fs::remove_dir_all(&live).map_err(|error| error.to_string())?;
            drop(guard);
            let _shadow = done_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|error| error.to_string())?;
            handle
                .join()
                .map_err(|_| "constructor thread panicked".to_string())?;
        }
        Ok(())
    }

    #[test]
    fn competing_process_cannot_clean_a_live_root_transaction() -> Result<(), String> {
        const ROOT_ENV: &str = "EKO_TASK_RUNTIME_ROOT_LEASE_CHILD";
        if let Some(root) = std::env::var_os(ROOT_ENV) {
            let shadow = FileTaskShadow::new_unbound_for_test(PathBuf::from(&root));
            assert!(shadow.root_authority().is_err());
            assert!(PathBuf::from(root).join(".preparing-live").exists());
            return Ok(());
        }
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let _owner = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".preparing-live"))
            .map_err(|error| error.to_string())?;
        let output = std::process::Command::new(
            std::env::current_exe().map_err(|error| error.to_string())?,
        )
        .arg("tasks::task_runtime::file_shadow::tests::competing_process_cannot_clean_a_live_root_transaction")
        .arg("--exact")
        .arg("--nocapture")
        .env(ROOT_ENV, temp.path())
        .output()
        .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "root lease child failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        assert!(temp.path().join(".preparing-live").exists());
        Ok(())
    }

    #[test]
    fn shadow_retries_root_lease_after_competing_process_exits() -> Result<(), String> {
        const ROOT_ENV: &str = "EKO_TASK_RUNTIME_ROOT_RETRY_CHILD";
        const READY_ENV: &str = "EKO_TASK_RUNTIME_ROOT_RETRY_READY";
        const RELEASE_ENV: &str = "EKO_TASK_RUNTIME_ROOT_RETRY_RELEASE";
        if let (Some(root), Some(ready), Some(release)) = (
            std::env::var_os(ROOT_ENV),
            std::env::var_os(READY_ENV),
            std::env::var_os(RELEASE_ENV),
        ) {
            let shadow =
                FileTaskShadow::new(PathBuf::from(root)).map_err(|error| error.to_string())?;
            shadow.root_authority().map_err(|error| error.to_string())?;
            std::fs::write(&ready, b"ready").map_err(|error| error.to_string())?;
            let release = PathBuf::from(release);
            for _ in 0..500 {
                if release.exists() {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            return Err("root lease child release timed out".to_string());
        }
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("leased-root");
        let ready = temp.path().join("ready");
        let release = temp.path().join("release");
        let mut child = std::process::Command::new(
            std::env::current_exe().map_err(|error| error.to_string())?,
        )
        .arg("tasks::task_runtime::file_shadow::tests::shadow_retries_root_lease_after_competing_process_exits")
        .arg("--exact")
        .env(ROOT_ENV, &root)
        .env(READY_ENV, &ready)
        .env(RELEASE_ENV, &release)
        .spawn()
        .map_err(|error| error.to_string())?;
        for _ in 0..500 {
            if ready.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !ready.exists() {
            let _killed = child.kill();
            return Err("root lease child did not become ready".to_string());
        }
        let retrying = FileTaskShadow::new_unbound_for_test(&root);
        assert!(retrying.root_authority().is_err());
        std::fs::write(&release, b"release").map_err(|error| error.to_string())?;
        let status = child.wait().map_err(|error| error.to_string())?;
        assert!(status.success());
        retrying
            .root_authority()
            .map_err(|error| error.to_string())?;
        assert_eq!(
            retrying
                .append_event_line(
                    "retry",
                    None,
                    None,
                    RuntimeEventKind::RunCreated,
                    serde_json::json!({"goal": "retry lease"}),
                )
                .map_err(|error| error.to_string())?
                .seq,
            1
        );
        Ok(())
    }

    #[test]
    fn fresh_nested_root_is_durably_created_before_first_publication() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("nested/one/two/tasks");
        let shadow = FileTaskShadow::new(&root).map_err(|error| error.to_string())?;
        run_created(&shadow, "fresh").map_err(|error| error.to_string())?;
        assert!(root.is_dir());
        assert!(root.join("fresh/events.jsonl").is_file());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn final_root_symlink_is_rejected_without_a_second_authority() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real = temp.path().join("real");
        std::fs::create_dir(&real).map_err(|error| error.to_string())?;
        let owner = FileTaskShadow::new(&real).map_err(|error| error.to_string())?;
        let owner_authority = owner.root_authority().map_err(|error| error.to_string())?;
        let alias = temp.path().join("alias");
        symlink(&real, &alias).map_err(|error| error.to_string())?;
        let aliased = FileTaskShadow::new_unbound_for_test(&alias);
        assert!(aliased.root_authority().is_err());
        assert_eq!(
            owner_authority.root(),
            std::fs::canonicalize(&real)
                .map_err(|error| error.to_string())?
                .as_path()
        );
        Ok(())
    }

    #[test]
    fn deletion_barrier_failure_leaves_cold_cleanup_tombstone() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "barrier-delete").map_err(|error| error.to_string())?;
        shadow.fail_root_sync_on_call_for_test(2);
        let error = shadow
            .remove_runs(&["barrier-delete".to_string()])
            .err()
            .ok_or_else(|| "injected deletion barrier unexpectedly succeeded".to_string())?;
        assert!(matches!(
            error,
            ShadowError::CommittedDeletionDegraded { .. }
        ));
        assert!(!temp.path().join("barrier-delete").exists());
        assert!(
            std::fs::read_dir(temp.path())
                .map_err(|error| error.to_string())?
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".deleting-"))
        );
        drop(shadow);
        let cold = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        assert!(
            !cold
                .list_run_ids()
                .map_err(|error| error.to_string())?
                .iter()
                .any(|run_id| run_id == "barrier-delete")
        );
        assert!(
            !std::fs::read_dir(temp.path())
                .map_err(|error| error.to_string())?
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".deleting-"))
        );
        Ok(())
    }

    #[test]
    fn degraded_initial_publication_still_dispatches_committed_hooks_in_order() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let observed = Arc::new(Mutex::new(Vec::new()));
        let hook_observed = Arc::clone(&observed);
        assert!(shadow.try_attach_event_hook(Arc::new(move |event| {
            hook_observed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event.seq);
        })));
        let timestamp = chrono::Utc::now();
        let events = vec![
            RuntimeJournalEvent::new(
                "publish-degraded",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": "publish degraded",
                    "goal_revision": 1,
                    "goal_sha256": crate::tasks::task_runtime::task_goal_sha256("publish degraded"),
                    "domain_profile": "general",
                    "workspace_id": "workspace",
                    "conversation_id": "conversation",
                    "root_message_id": "message",
                    "route": "complex",
                    "attended_mode": "attended",
                }),
                timestamp,
            ),
            RuntimeJournalEvent::new(
                "publish-degraded",
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                serde_json::json!({
                    "reason": "initial",
                    "base_revision": 0,
                    "skipped_task_ids": [],
                    "plan": PlanRevision {
                        plan_id: "plan".to_string(),
                        run_id: "publish-degraded".to_string(),
                        revision: 1,
                        domain_profile: DomainProfile::General,
                        goal_revision: 1,
                        goal_sha256: crate::tasks::task_runtime::task_goal_sha256("publish degraded"),
                        assumptions: Vec::new(),
                        risks: Vec::new(),
                        execution_mode: ExecutionMode::Sequential,
                        tasks: Vec::new(),
                    },
                }),
                timestamp,
            ),
        ];
        shadow.fail_root_sync_on_call_for_test(1);
        assert!(matches!(
            shadow.publish_initial_event_batch("publish-degraded", events),
            Err(ShadowError::CommittedPublicationDegraded { .. })
        ));
        assert_eq!(
            observed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            &[1, 2]
        );
        assert!(temp.path().join("publish-degraded/events.jsonl").is_file());
        let journal = std::fs::read_to_string(temp.path().join("publish-degraded/events.jsonl"))
            .map_err(|error| error.to_string())?;
        assert_eq!(journal.lines().count(), 1);
        let frame: serde_json::Value = serde_json::from_str(
            journal
                .lines()
                .next()
                .ok_or_else(|| "initial publication frame missing".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            frame
                .get("records")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        drop(shadow);
        assert_eq!(
            FileTaskShadow::new(temp.path())
                .map_err(|error| error.to_string())?
                .read_events("publish-degraded")
                .map_err(|error| error.to_string())?
                .len(),
            2
        );
        Ok(())
    }

    #[test]
    fn degraded_initial_batch_stays_hidden_and_dispatches_no_hooks() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let observed = Arc::new(AtomicUsize::new(0));
        let hook_observed = Arc::clone(&observed);
        assert!(shadow.try_attach_event_hook(Arc::new(move |_event| {
            hook_observed.fetch_add(1, Ordering::SeqCst);
        })));
        let timestamp = chrono::Utc::now();
        let goal = "hidden degraded publication";
        let events = vec![
            RuntimeJournalEvent::new(
                "hidden-degraded",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": goal,
                    "goal_revision": 1,
                    "goal_sha256": crate::tasks::task_runtime::task_goal_sha256(goal),
                    "domain_profile": "general",
                    "workspace_id": "workspace",
                    "conversation_id": "conversation",
                    "root_message_id": "message",
                    "route": "complex",
                    "attended_mode": "attended",
                }),
                timestamp,
            ),
            RuntimeJournalEvent::new(
                "hidden-degraded",
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                serde_json::json!({
                    "reason": "initial",
                    "base_revision": 0,
                    "skipped_task_ids": [],
                    "plan": PlanRevision {
                        plan_id: "plan".to_string(),
                        run_id: "hidden-degraded".to_string(),
                        revision: 1,
                        domain_profile: DomainProfile::General,
                        goal_revision: 1,
                        goal_sha256: crate::tasks::task_runtime::task_goal_sha256(goal),
                        assumptions: Vec::new(),
                        risks: Vec::new(),
                        execution_mode: ExecutionMode::Sequential,
                        tasks: Vec::new(),
                    },
                }),
                timestamp,
            ),
        ];
        shadow.fail_next_initial_batch_durability_for_test();
        assert!(matches!(
            shadow.publish_initial_event_batch("hidden-degraded", events),
            Err(ShadowError::InitialBatchDurabilityDegraded { .. })
        ));
        assert_eq!(observed.load(Ordering::SeqCst), 0);
        assert!(!temp.path().join("hidden-degraded").exists());
        assert!(
            std::fs::read_dir(temp.path())
                .map_err(|error| error.to_string())?
                .filter_map(Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".preparing-"))
        );
        Ok(())
    }

    #[test]
    fn append_failure_closes_all_aliases_and_reopen_reuses_sequence() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let second = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&first, "poison").map_err(|error| error.to_string())?;
        let first_alias = first
            .authority("poison", false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "first alias missing".to_string())?;
        let second_alias = second
            .authority("poison", false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "second alias missing".to_string())?;
        let path = temp.path().join("poison/events.jsonl");
        let backup = temp.path().join("poison/events.backup");
        std::fs::rename(&path, &backup).map_err(|error| error.to_string())?;
        std::fs::create_dir(&path).map_err(|error| error.to_string())?;
        assert!(
            first
                .append_event_line(
                    "poison",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"fails": true}),
                )
                .is_err()
        );
        for alias in [first_alias, second_alias] {
            assert!(matches!(
                alias.append(RuntimeJournalEvent::for_append(
                    "poison",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"stale": true}),
                )),
                Err(ShadowError::AuthorityClosed(_))
            ));
        }
        std::fs::remove_dir(&path).map_err(|error| error.to_string())?;
        std::fs::rename(&backup, &path).map_err(|error| error.to_string())?;
        assert_eq!(
            second
                .append_event_line(
                    "poison",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"reopened": true}),
                )
                .map_err(|error| error.to_string())?
                .seq,
            2
        );
        Ok(())
    }

    #[test]
    fn post_commit_validation_failure_closes_aliases_and_cold_recovery_sees_full_batch()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "post-commit").map_err(|error| error.to_string())?;
        let observed = Arc::new(AtomicUsize::new(0));
        let hook_observed = Arc::clone(&observed);
        assert!(shadow.try_attach_event_hook(Arc::new(move |_event| {
            hook_observed.fetch_add(1, Ordering::SeqCst);
        })));
        let alias = shadow
            .authority("post-commit", false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "post-commit authority missing".to_string())?;
        alias.fail_next_post_commit_validation_for_test();

        assert!(matches!(
            shadow.append_event_line(
                "post-commit",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({ "marker": "committed-before-validation" }),
            ),
            Err(ShadowError::BatchOutcomeUnknown { detail, .. })
                if detail.contains("injected post-commit")
        ));
        assert_eq!(observed.load(Ordering::SeqCst), 0);
        assert!(matches!(
            alias.append(RuntimeJournalEvent::for_append(
                "post-commit",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({ "stale": true }),
            )),
            Err(ShadowError::AuthorityClosed(_))
        ));

        let cold = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let events = cold
            .read_events("post-commit")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event
                        .payload
                        .get("marker")
                        .and_then(serde_json::Value::as_str)
                        == Some("committed-before-validation")
                })
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn lru_bounds_idle_authorities_and_pins_active_or_degraded_entries() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "active").map_err(|error| error.to_string())?;
        run_created(&shadow, "debt").map_err(|error| error.to_string())?;
        let active = shadow
            .authority("active", false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "active authority missing".to_string())?;
        let active_guard = active.lock_operation_for_test();
        let debt = shadow
            .authority("debt", false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "debt authority missing".to_string())?;
        debt.mark_durability_debt_for_test();
        drop(debt);
        for index in 0..(MAX_CACHED_RUN_AUTHORITIES + 24) {
            run_created(&shadow, &format!("historical-{index}"))
                .map_err(|error| error.to_string())?;
        }
        assert!(shadow.cached_authority_count_for_test() <= MAX_CACHED_RUN_AUTHORITIES + 1);
        assert!(shadow.has_cached_authority_for_test("active"));
        assert!(shadow.has_cached_authority_for_test("debt"));
        assert_eq!(
            shadow
                .read_events("historical-0")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        drop(active_guard);
        drop(active);
        shadow
            .append_event_line(
                "debt",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({"barrier": "cleared"}),
            )
            .map_err(|error| error.to_string())?;
        for index in 0..8 {
            run_created(&shadow, &format!("later-{index}")).map_err(|error| error.to_string())?;
        }
        assert!(shadow.cached_authority_count_for_test() <= MAX_CACHED_RUN_AUTHORITIES);
        let second = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        for index in 0..(MAX_CACHED_RUN_AUTHORITIES + 24) {
            second
                .read_events(&format!("historical-{index}"))
                .map_err(|error| error.to_string())?;
        }
        assert!(second.cached_authority_count_for_test() <= MAX_CACHED_RUN_AUTHORITIES);
        Ok(())
    }

    #[test]
    fn projection_only_orphan_is_not_a_task_run() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let orphan = temp.path().join("orphan");
        std::fs::create_dir_all(&orphan).map_err(|error| error.to_string())?;
        std::fs::write(orphan.join("plan.json"), b"{}").map_err(|error| error.to_string())?;
        std::fs::write(orphan.join("run-state.json"), b"{}").map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        assert!(
            !shadow
                .list_run_ids()
                .map_err(|error| error.to_string())?
                .iter()
                .any(|run_id| run_id == "orphan")
        );
        assert!(
            shadow
                .read_plan("orphan")
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert!(
            shadow
                .read_run_state("orphan")
                .map_err(|error| error.to_string())?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn event_batch_is_all_or_none_when_the_authority_is_unwritable() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        run_created(&shadow, "atomic").map_err(|error| error.to_string())?;
        let before = shadow
            .read_events("atomic")
            .map_err(|error| error.to_string())?;
        drop(shadow);
        let event_path = temp.path().join("atomic/events.jsonl");
        let backup = temp.path().join("atomic/events.backup");
        std::fs::rename(&event_path, &backup).map_err(|error| error.to_string())?;
        std::fs::create_dir(&event_path).map_err(|error| error.to_string())?;

        let broken = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let result = broken.append_event_batch(
            "atomic",
            vec![
                RuntimeJournalEvent::for_append(
                    "atomic",
                    None,
                    None,
                    RuntimeEventKind::RunGoalUpdated,
                    serde_json::json!({"operation": "first"}),
                ),
                RuntimeJournalEvent::for_append(
                    "atomic",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"operation": "second"}),
                ),
            ],
        );
        assert!(result.is_err());
        drop(broken);
        std::fs::remove_dir(&event_path).map_err(|error| error.to_string())?;
        std::fs::rename(&backup, &event_path).map_err(|error| error.to_string())?;
        let cold = FileTaskShadow::new(temp.path()).map_err(|error| error.to_string())?;
        let after = cold
            .read_events("atomic")
            .map_err(|error| error.to_string())?;
        assert_eq!(after.len(), before.len());
        assert!(!after.iter().any(|event| {
            matches!(
                event
                    .payload
                    .get("operation")
                    .and_then(serde_json::Value::as_str),
                Some("first" | "second")
            )
        }));
        Ok(())
    }
}
