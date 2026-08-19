//! File-based authoritative store (U1c phase-0/0bc).
//!
//! The file system (`events.jsonl` + `plan.json`) is the read/write authority
//! for all task data. SQL was retired in 0bc step 5.
//!
//! Layout: `{root}/{run_id}/events.jsonl` (append-only authority), `plan.json`
//! and `run-state.json` (snapshots), plus discardable `checkpoint.json`.
//! `root` defaults to `~/.eko/tasks/` (global, spec §2 path A).

use std::path::{Path, PathBuf};

use super::checkpoint::RuntimeCheckpoint;
use super::event_rebuild::EventFoldState;
use super::types::{PlanRevision, RunStateSnapshot, RuntimeEventKind, RuntimeTaskEvent};

/// Shadow writer for one root directory. Cheap to clone (wraps a root path; the
/// append lock is per-run via the `events.jsonl` file handle being held briefly).
#[derive(Clone)]
pub struct FileTaskShadow {
    root: std::sync::Arc<std::sync::RwLock<PathBuf>>,
    /// In-memory seq cache (run_id → last assigned seq) to avoid reading the
    /// tail of `events.jsonl` on every append. Seeded lazily from the final
    /// durable event on first append per run, so it self-heals across restarts.
    /// Shared across clones via `Arc` so all clones of this shadow agree on seq.
    /// Contention is low: the store holds
    /// a single `Mutex` serializing all writes (the in-memory usage/conv-event
    /// mutexes, not a DB connection).
    seq_cache: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, i64>>>,
    /// Per-run write locks shared by event append and snapshot rewrite.
    /// `append_event_line` holds the lock across seq allocation → append →
    /// cache update; `rewrite_plan` acquires it again after append returns.
    /// This prevents duplicate seq allocation and stale plan.json renames.
    /// Different runs still run in parallel; only same-run writes serialize.
    run_write_locks: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>,
    >,
    /// Optional synchronous event hook invoked after each event is successfully
    /// appended. The hook receives the fully-formed `RuntimeTaskEvent`. Used by
    /// the application-layer HookEventDispatcher to translate RuntimeEventKind
    /// into framework HookEvents (TaskCreated/Started/Completed(status),
    /// SubagentStop(status)) without polluting the sync store with async logic.
    /// The callback must be cheap (spawn-and-detach); it runs under the per-run
    /// write lock, so blocking here blocks all same-run writes.
    ///
    /// `OnceLock` so it can be attached once, post-construction: the store is
    /// built early in bootstrap (before bridges exist), then the dispatcher is
    /// attached once the agent + bridges are ready. Shared via Arc across clones.
    #[allow(clippy::type_complexity)]
    // Arc<OnceLock<Arc<dyn Fn>>> — structural, mirrors run_write_locks above
    event_hook: std::sync::Arc<
        std::sync::OnceLock<std::sync::Arc<dyn Fn(&RuntimeTaskEvent) + Send + Sync>>,
    >,
    #[cfg(test)]
    fail_initial_publish_before_rename: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl FileTaskShadow {
    /// Create a shadow rooted at `root`. The directory is created lazily on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let shadow = Self {
            root: std::sync::Arc::new(std::sync::RwLock::new(root.into())),
            seq_cache: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_write_locks: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            event_hook: std::sync::Arc::new(std::sync::OnceLock::new()),
            #[cfg(test)]
            fail_initial_publish_before_rename: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
        };
        shadow.recover_interrupted_transactions();
        shadow
    }

    /// Attach a synchronous event hook fired after each successful append.
    ///
    /// Idempotent: the first call wins; subsequent calls are ignored (returns
    /// false). This lets bootstrap attach the dispatcher once bridges exist
    /// without racing. The hook receives the persisted `RuntimeTaskEvent` and
    /// MUST be cheap (spawn-and-detach) because it runs under the per-run
    /// write lock. This is the single injection point that lets the application
    /// translate the event-sourced RuntimeEventKind stream into framework
    /// HookEvents without making the store async.
    pub fn try_attach_event_hook(
        &self,
        hook: std::sync::Arc<dyn Fn(&RuntimeTaskEvent) + Send + Sync>,
    ) -> bool {
        self.event_hook.set(hook).is_ok()
    }

    /// Default shadow root: `~/.eko/tasks/`.
    pub fn default_root() -> PathBuf {
        echo_agent::paths::user_data_path("tasks")
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.root
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn rebind_root(&self, root: PathBuf) {
        *self
            .root
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = root;
        self.seq_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.run_write_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.recover_interrupted_transactions();
    }

    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.root().join(run_id)
    }

    fn events_path(&self, run_id: &str) -> PathBuf {
        self.run_dir(run_id).join("events.jsonl")
    }

    fn plan_path(&self, run_id: &str) -> PathBuf {
        self.run_dir(run_id).join("plan.json")
    }

    fn run_state_path(&self, run_id: &str) -> PathBuf {
        self.run_dir(run_id).join("run-state.json")
    }

    fn checkpoint_path(&self, run_id: &str) -> PathBuf {
        self.run_dir(run_id).join("checkpoint.json")
    }

    /// Settle hidden task-publication and deletion directories left by a process
    /// that ended inside a file transaction. Product run enumeration never
    /// treats these directories as TaskRuns.
    fn recover_interrupted_transactions(&self) {
        let root = self.root();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(%error, path = %root.display(), "failed to inspect task publication transactions");
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(%error, path = %root.display(), "failed to inspect task publication transaction entry");
                    continue;
                }
            };
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    tracing::warn!(%error, path = %entry.path().display(), "failed to inspect task publication transaction type");
                    continue;
                }
            };
            if file_type.is_dir()
                && (name.starts_with(".preparing-") || name.starts_with(".deleting-"))
                && let Err(error) = std::fs::remove_dir_all(entry.path())
            {
                tracing::warn!(%error, path = %entry.path().display(), "failed to remove stale task file transaction");
            }
        }
    }

    /// Publish a TaskRun's complete first generation with one directory rename.
    /// The supplied event batch is already framework-validated; both derived
    /// projections are rebuilt before the run becomes enumerable.
    pub(crate) fn publish_initial_event_batch(
        &self,
        run_id: &str,
        events: &[RuntimeTaskEvent],
    ) -> Result<(), ShadowError> {
        if events.is_empty() {
            return Err(ShadowError::Encode(
                "initial task publication requires at least one event".to_string(),
            ));
        }
        for (index, event) in events.iter().enumerate() {
            let expected_seq = i64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    ShadowError::Encode("initial event sequence overflow".to_string())
                })?;
            if event.run_id != run_id || event.seq != expected_seq {
                return Err(ShadowError::Encode(format!(
                    "invalid initial event at position {expected_seq}: run '{}', seq {}",
                    event.run_id, event.seq
                )));
            }
        }
        if events.first().map(|event| event.event_type) != Some(RuntimeEventKind::RunCreated)
            || !events
                .iter()
                .any(|event| event.event_type == RuntimeEventKind::PlanRevisionCommitted)
        {
            return Err(ShadowError::Encode(
                "initial task publication requires RunCreated and PlanRevisionCommitted"
                    .to_string(),
            ));
        }

        let rebuilt = super::event_rebuild::rebuild_plan_from_events(events)
            .map_err(|error| ShadowError::Rebuild(error.to_string()))?;
        let mut events_jsonl = Vec::new();
        for event in events {
            serde_json::to_writer(&mut events_jsonl, event)
                .map_err(|error| ShadowError::Encode(error.to_string()))?;
            events_jsonl.push(b'\n');
        }
        let plan_json = serde_json::to_vec_pretty(&rebuilt.plan_revision())
            .map_err(|error| ShadowError::Encode(error.to_string()))?;
        let state_json = serde_json::to_vec_pretty(&rebuilt.run_state())
            .map_err(|error| ShadowError::Encode(error.to_string()))?;

        let lock = self.run_write_lock(run_id);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        let root = self.root();
        std::fs::create_dir_all(&root).map_err(|error| ShadowError::Io(error.to_string()))?;
        let final_directory = self.run_dir(run_id);
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
        let stage_result = (|| -> Result<(), ShadowError> {
            write_synced(&staging_directory.join("events.jsonl"), &events_jsonl)
                .map_err(|error| ShadowError::Io(error.to_string()))?;
            write_synced(&staging_directory.join("plan.json"), &plan_json)
                .map_err(|error| ShadowError::Io(error.to_string()))?;
            write_synced(&staging_directory.join("run-state.json"), &state_json)
                .map_err(|error| ShadowError::Io(error.to_string()))?;
            sync_directory(&staging_directory)
                .map_err(|error| ShadowError::Io(error.to_string()))?;
            if simulate_crash {
                return Err(ShadowError::Io(
                    "injected crash before initial run publication".to_string(),
                ));
            }
            std::fs::rename(&staging_directory, &final_directory)
                .map_err(|error| ShadowError::Io(error.to_string()))?;
            if let Err(error) = sync_directory(&root) {
                tracing::warn!(%error, path = %root.display(), "task publication is visible but parent directory sync failed");
            }
            Ok(())
        })();
        if stage_result.is_err() {
            if !simulate_crash && let Err(error) = std::fs::remove_dir_all(&staging_directory) {
                tracing::warn!(%error, path = %staging_directory.display(), "failed to remove aborted task publication transaction");
            }
            return stage_result;
        }

        let last_seq = events.last().map(|event| event.seq).unwrap_or_default();
        self.seq_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(run_id.to_string(), last_seq);
        if let Some(hook) = self.event_hook.get() {
            for event in events {
                hook(event);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_initial_publish_before_rename(&self) {
        self.fail_initial_publish_before_rename
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    // ── 0bc step-2: file-authority write path (replaces SQL INSERT + flush) ──

    /// Append one enriched event to `events.jsonl` for `run_id`, assigning it
    /// the next seq (1-based). Returns the fully
    /// formed `RuntimeTaskEvent` (with seq + timestamp) that was written.
    ///
    /// This is the file-authority write primitive: store write methods call
    /// this instead of a database insert, then call [`rewrite_plan`] to
    /// incrementally refresh the snapshots. seq is per-run (each run has its own
    /// `events.jsonl`), so appending to run B does not advance run A's seq.
    ///
    /// Atomicity: the append is serialized by the store mutex (single writer),
    /// and the line is written with a trailing newline. A crash mid-append can
    /// at worst leave the last partial line; the next append repairs that torn
    /// tail before allocating its sequence.
    pub fn append_event_line(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        step_id: Option<&str>,
        event_type: super::types::RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Result<RuntimeTaskEvent, ShadowError> {
        // Hold the per-run write lock across seq alloc → append → cache bump.
        // This closes the duplicate-seq race that arose when callers entered
        // `next_seq` concurrently and each observed the same cached value
        // before any append landed (observed in production events.jsonl as
        // repeated seq 8/9/30/57/63-66). Different runs still run in
        // parallel; only same-run writes serialize.
        let lock = self.run_write_lock(run_id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        let dir = self.run_dir(run_id);
        std::fs::create_dir_all(&dir).map_err(|e| ShadowError::Io(e.to_string()))?;
        repair_torn_tail(&self.events_path(run_id))?;

        // seq = last assigned + 1 (1-based). Cached in memory per run to avoid
        // reading the events.jsonl tail on every append; seeded from the final
        // durable event on first touch per run so it self-heals across restarts.
        let next_seq = self.next_seq(run_id)?;

        let event = RuntimeTaskEvent {
            seq: next_seq,
            run_id: run_id.to_string(),
            task_id: task_id.map(str::to_string),
            step_id: step_id.map(str::to_string),
            event_type,
            payload,
            timestamp: chrono::Utc::now(),
        };

        // Append one JSON line. Ensure a trailing newline so the file is a
        // well-formed JSONL stream (each event on its own line).
        let mut line =
            serde_json::to_string(&event).map_err(|e| ShadowError::Encode(e.to_string()))?;
        line.push('\n');
        append_line(&self.events_path(run_id), line.as_bytes())
            .map_err(|e| ShadowError::Io(e.to_string()))?;

        // Advance the in-memory cache so the next append doesn't re-read the file.
        if let Ok(mut cache) = self.seq_cache.lock() {
            cache.insert(run_id.to_string(), next_seq);
        }
        // Fire the event hook (if attached) AFTER the cache bump and OUTSIDE
        // the seq-allocation critical section, but still under the per-run
        // write lock. The bounded HookEventDispatcher normally makes this a
        // cheap enqueue; when saturated it deliberately applies backpressure
        // instead of dropping lifecycle events. This is the single point
        // the HookEventDispatcher observes every RuntimeEventKind transition
        // (plan revision commit, task status change, subagent assigned/
        // released, run status change) and translates it into framework
        // HookEvents. Run while `event` is still owned so the borrow is short.
        if let Some(hook) = self.event_hook.get() {
            hook(&event);
        }
        Ok(event)
    }

    /// Get-or-create the per-run write lock Arc. The map itself is guarded by
    /// a short-lived Mutex so callers never see a torn HashMap. Entries are
    /// never removed on Drop (no remove-on-drop race); they live as long as
    /// the FileTaskShadow, which is fine because the keyspace is run_ids and
    /// the map is bounded by total runs ever written.
    fn run_write_lock(&self, run_id: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
        // Mutex::lock only fails on poison; recover the inner guard rather than
        // panicking (matches the existing seq_cache pattern in this module).
        let mut map = self
            .run_write_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.entry(run_id.to_string())
            .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
            .clone()
    }

    /// Refresh the projections affected by the latest event.
    ///
    /// The method keeps its historical name while callers migrate, but
    /// `plan.json` now contains only the plan specification and
    /// `run-state.json` contains mutable execution state. Events that affect
    /// neither projection (tool traces, reviews, artifacts) perform no rewrite.
    ///
    /// If the event stream has no `RunCreated` yet (e.g. an orphan review
    /// written before the run was created — SQL tolerated this), there is no
    /// plan to snapshot, so this is a no-op rather than an error. The events
    /// themselves are still durably appended to `events.jsonl`.
    pub fn rewrite_plan(&self, run_id: &str) -> Result<(), ShadowError> {
        self.refresh_projections(run_id, false).map(|_| ())
    }

    #[cfg(test)]
    pub(crate) fn rewrite_plan_with_stats(
        &self,
        run_id: &str,
    ) -> Result<ProjectionRefreshStats, ShadowError> {
        self.refresh_projections(run_id, false)
    }

    /// Ensure snapshots include the complete durable event tail.
    ///
    /// A valid checkpoint with no suffix proves that snapshots written before
    /// it reached the same event seq, so the common read path performs no
    /// projection writes. A missing/invalid checkpoint or non-empty suffix is
    /// repaired from `events.jsonl` before the caller reads a snapshot.
    pub(crate) fn ensure_projections_current(&self, run_id: &str) -> Result<(), ShadowError> {
        self.refresh_projections(run_id, true).map(|_| ())
    }

    fn refresh_projections(
        &self,
        run_id: &str,
        skip_write_when_checkpoint_is_current: bool,
    ) -> Result<ProjectionRefreshStats, ShadowError> {
        // `append_event_line` releases its guard before returning, so this is
        // not a re-entrant lock acquisition. Wait for any same-run append or
        // rewrite to finish; proceeding after `try_lock` returns WouldBlock
        // would rebuild and rename plan.json without serialization.
        let lock = self.run_write_lock(run_id);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());

        let (mut state, events, used_checkpoint) = match self.load_checkpoint_suffix(run_id) {
            Some(Ok((checkpoint, suffix))) => (checkpoint.state, suffix, true),
            Some(Err(error)) => {
                tracing::warn!(run_id, %error, "discarding invalid TaskRuntime checkpoint");
                (EventFoldState::default(), self.read_events(run_id)?, false)
            }
            None => (EventFoldState::default(), self.read_events(run_id)?, false),
        };
        if events.is_empty() && !used_checkpoint {
            return Ok(ProjectionRefreshStats {
                used_checkpoint,
                folded_events: 0,
                seq: state.last_seq(),
            });
        }
        if skip_write_when_checkpoint_is_current && used_checkpoint && events.is_empty() {
            return Ok(ProjectionRefreshStats {
                used_checkpoint,
                folded_events: 0,
                seq: state.last_seq(),
            });
        }
        if !used_checkpoint {
            validate_event_suffix(run_id, 0, &events)?;
        }
        let affects_plan = events.iter().any(event_affects_plan)
            || (events.is_empty()
                && state
                    .rebuilt_plan()
                    .is_ok_and(|plan| plan.plan.revision > 0));
        let affects_run_state = events.iter().any(event_affects_run_state)
            || (events.is_empty() && state.run_id().is_some());
        state.apply_events(&events);
        let rebuilt = match state.rebuilt_plan() {
            Ok(r) => r,
            Err(super::event_rebuild::RebuildError::NoRunCreated) => {
                // No RunCreated in the stream. On the live file path every
                // write that reaches here has either just appended RunCreated
                // (create_run) or operates on an existing run — so hitting this
                // branch means an orphan write (e.g. add_review before
                // create_run, which SQL tolerated) or a corrupted/partial
                // events.jsonl. Events are still authoritative on disk; we skip
                // the plan.json snapshot but log so this stays diagnosable
                // rather than a silent no-op.
                tracing::warn!(
                    run_id = %run_id,
                    event_count = events.len(),
                    "rewrite_plan: no RunCreated in event stream, skipping plan.json snapshot \
                     (orphan write or corrupted events.jsonl)"
                );
                return Ok(ProjectionRefreshStats {
                    used_checkpoint,
                    folded_events: events.len(),
                    seq: state.last_seq(),
                });
            }
        };
        std::fs::create_dir_all(self.run_dir(run_id))
            .map_err(|error| projection_degraded(state.last_seq(), error))?;
        if affects_plan {
            let plan_json = serde_json::to_string_pretty(&rebuilt.plan_revision())
                .map_err(|error| projection_degraded(state.last_seq(), error))?;
            atomic_write(&self.plan_path(run_id), plan_json.as_bytes())
                .map_err(|error| projection_degraded(state.last_seq(), error))?;
        }
        if affects_run_state {
            let state_json = serde_json::to_string_pretty(&rebuilt.run_state())
                .map_err(|error| projection_degraded(state.last_seq(), error))?;
            atomic_write(&self.run_state_path(run_id), state_json.as_bytes())
                .map_err(|error| projection_degraded(state.last_seq(), error))?;
        }
        let event_byte_offset = std::fs::metadata(self.events_path(run_id))
            .map_err(|error| projection_degraded(state.last_seq(), error))?
            .len();
        let checkpoint_seq = state.last_seq();
        let checkpoint = RuntimeCheckpoint::new(run_id, event_byte_offset, state)
            .map_err(|error| projection_degraded(checkpoint_seq, error))?;
        let checkpoint_json = serde_json::to_vec(&checkpoint)
            .map_err(|error| projection_degraded(checkpoint.seq, error))?;
        atomic_write(&self.checkpoint_path(run_id), &checkpoint_json)
            .map_err(|error| projection_degraded(checkpoint.seq, error))?;
        Ok(ProjectionRefreshStats {
            used_checkpoint,
            folded_events: events.len(),
            seq: checkpoint.seq,
        })
    }

    fn load_checkpoint_suffix(
        &self,
        run_id: &str,
    ) -> Option<Result<(RuntimeCheckpoint, Vec<RuntimeTaskEvent>), ShadowError>> {
        let path = self.checkpoint_path(run_id);
        if !path.exists() {
            return None;
        }
        Some((|| {
            let bytes = std::fs::read(&path).map_err(|error| {
                ShadowError::Rebuild(format!("checkpoint read failed: {error}"))
            })?;
            let checkpoint =
                serde_json::from_slice::<RuntimeCheckpoint>(&bytes).map_err(|error| {
                    ShadowError::Rebuild(format!("checkpoint decode failed: {error}"))
                })?;
            checkpoint.validate(run_id).map_err(|error| {
                ShadowError::Rebuild(format!("checkpoint validation failed: {error}"))
            })?;
            let suffix = self.read_events_from_offset(run_id, checkpoint.event_byte_offset)?;
            validate_event_suffix(run_id, checkpoint.seq, &suffix)?;
            Ok((checkpoint, suffix))
        })())
    }

    /// Compute the next seq for `run_id`: last assigned + 1 (1-based).
    ///
    /// Uses the in-memory `seq_cache` on the steady path. On first touch after
    /// restart, reads only the final complete JSONL event rather than parsing
    /// the entire history. Returns 1 if the file does not exist yet.
    fn next_seq(&self, run_id: &str) -> Result<i64, ShadowError> {
        // Fast path: cached.
        if let Ok(cache) = self.seq_cache.lock()
            && let Some(&last) = cache.get(run_id)
        {
            return last
                .checked_add(1)
                .ok_or_else(|| ShadowError::Rebuild("event seq overflow".to_string()));
        }
        // Restart path: seed from the final durable event in bounded backwards
        // reads. Event lines may be large, so the scan continues by block until
        // it finds the preceding newline without assuming a maximum line size.
        let last = read_last_event(&self.events_path(run_id))?;
        match last {
            Some(event) if event.run_id != run_id => Err(ShadowError::Rebuild(format!(
                "last event belongs to run {}, expected {run_id}",
                event.run_id
            ))),
            Some(event) => event
                .seq
                .checked_add(1)
                .ok_or_else(|| ShadowError::Rebuild("event seq overflow".to_string())),
            None => Ok(1),
        }
    }

    /// Enumerate every run_id known to the file store: the directory names
    /// under `root` that contain an `events.jsonl` (or `plan.json`). Used by
    /// the collection-query read API (`list_runs` / `list_runs_in` / etc.) that
    /// replaces SQL `SELECT ... FROM tr_runs WHERE ...`.
    pub fn list_run_ids(&self) -> Result<Vec<String>, ShadowError> {
        let mut ids = Vec::new();
        let root = self.root();
        let read_dir = match std::fs::read_dir(&root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(ShadowError::Io(e.to_string())),
        };
        for entry in read_dir {
            let entry = entry.map_err(|e| ShadowError::Io(e.to_string()))?;
            let path = entry.path();
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(".preparing-"))
            {
                continue;
            }
            if !path.is_dir() {
                continue;
            }
            // A run dir is one that has events.jsonl or plan.json.
            let has_events = path.join("events.jsonl").exists();
            let has_plan = path.join("plan.json").exists();
            let has_run_state = path.join("run-state.json").exists();
            if !has_events && !has_plan && !has_run_state {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                ids.push(name.to_string());
            }
        }
        Ok(ids)
    }

    /// Hide a settled set of TaskRuns before removing their files. The durable
    /// conversation deletion transaction owns cross-store retries; this method
    /// keeps ordinary rename failures from exposing only part of this store's
    /// participant set.
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

        let locks = run_ids
            .iter()
            .map(|run_id| self.run_write_lock(run_id))
            .collect::<Vec<_>>();
        let guards = locks
            .iter()
            .map(|lock| lock.lock().unwrap_or_else(|error| error.into_inner()))
            .collect::<Vec<_>>();
        let root = self.root();
        std::fs::create_dir_all(&root).map_err(|error| ShadowError::Io(error.to_string()))?;
        let tombstone = root.join(format!(".deleting-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&tombstone).map_err(|error| ShadowError::Io(error.to_string()))?;

        let mut moved = Vec::<(PathBuf, PathBuf)>::new();
        for run_id in &run_ids {
            let source = self.run_dir(run_id);
            if !source.exists() {
                continue;
            }
            let target = tombstone.join(run_id);
            if let Err(error) = std::fs::rename(&source, &target) {
                let mut rollback_errors = Vec::new();
                for (original, staged) in moved.iter().rev() {
                    if let Err(rollback_error) = std::fs::rename(staged, original) {
                        rollback_errors.push(rollback_error.to_string());
                    }
                }
                if let Err(cleanup_error) = std::fs::remove_dir(&tombstone) {
                    rollback_errors.push(cleanup_error.to_string());
                }
                return Err(ShadowError::Io(format!(
                    "failed to stage run {run_id} for deletion: {error}; rollback errors: {}",
                    rollback_errors.join("; ")
                )));
            }
            moved.push((source, target));
        }
        drop(guards);

        for run_id in &run_ids {
            self.seq_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(run_id);
            self.run_write_locks
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(run_id);
        }
        if let Err(error) = std::fs::remove_dir_all(&tombstone) {
            tracing::warn!(path = %tombstone.display(), %error, "TaskRun deletion tombstone remains for startup cleanup");
        }
        Ok(())
    }

    /// Read the shadow plan.json for parity comparison. Returns None if not yet written.
    pub fn read_plan(&self, run_id: &str) -> Result<Option<PlanRevision>, ShadowError> {
        let path = self.plan_path(run_id);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| ShadowError::Io(e.to_string()))?;
        let plan: PlanRevision =
            serde_json::from_str(&text).map_err(|e| ShadowError::Decode(e.to_string()))?;
        Ok(Some(plan))
    }

    pub fn read_run_state(&self, run_id: &str) -> Result<Option<RunStateSnapshot>, ShadowError> {
        let path = self.run_state_path(run_id);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| ShadowError::Io(e.to_string()))?;
        let state: RunStateSnapshot =
            serde_json::from_str(&text).map_err(|e| ShadowError::Decode(e.to_string()))?;
        Ok(Some(state))
    }

    /// Read the shadow events.jsonl for parity comparison.
    pub fn read_events(&self, run_id: &str) -> Result<Vec<RuntimeTaskEvent>, ShadowError> {
        let path = self.events_path(run_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| ShadowError::Io(e.to_string()))?;
        decode_event_text(&text)
    }

    fn read_events_from_offset(
        &self,
        run_id: &str,
        offset: u64,
    ) -> Result<Vec<RuntimeTaskEvent>, ShadowError> {
        use std::io::{Read, Seek, SeekFrom};

        let path = self.events_path(run_id);
        let mut file = std::fs::File::open(&path).map_err(|error| {
            ShadowError::Rebuild(format!("checkpoint event read failed: {error}"))
        })?;
        let file_len = file
            .metadata()
            .map_err(|error| ShadowError::Rebuild(error.to_string()))?
            .len();
        if offset > file_len {
            return Err(ShadowError::Rebuild(format!(
                "checkpoint offset {offset} exceeds event length {file_len}"
            )));
        }
        if offset > 0 {
            file.seek(SeekFrom::Start(offset.saturating_sub(1)))
                .map_err(|error| ShadowError::Rebuild(error.to_string()))?;
            let mut boundary = [0_u8; 1];
            file.read_exact(&mut boundary)
                .map_err(|error| ShadowError::Rebuild(error.to_string()))?;
            if boundary.first().copied() != Some(b'\n') {
                return Err(ShadowError::Rebuild(
                    "checkpoint offset is not an event boundary".to_string(),
                ));
            }
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| ShadowError::Rebuild(error.to_string()))?;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|error| ShadowError::Rebuild(error.to_string()))?;
        decode_event_text(&text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectionRefreshStats {
    pub(crate) used_checkpoint: bool,
    pub(crate) folded_events: usize,
    pub(crate) seq: i64,
}

fn event_affects_plan(event: &RuntimeTaskEvent) -> bool {
    matches!(
        event.event_type,
        RuntimeEventKind::PlanRevisionCommitted | RuntimeEventKind::RequirementEvidenceRevalidated
    )
}

fn event_affects_run_state(event: &RuntimeTaskEvent) -> bool {
    let note_kind = event
        .payload
        .get("kind")
        .and_then(serde_json::Value::as_str);
    matches!(
        event.event_type,
        RuntimeEventKind::RunCreated
            | RuntimeEventKind::RunGoalUpdated
            | RuntimeEventKind::RequirementEvidenceInvalidated
            | RuntimeEventKind::RequirementEvidenceRevalidated
            | RuntimeEventKind::RequirementSkipped
            | RuntimeEventKind::RunStatusChanged
            | RuntimeEventKind::RunAttachmentsUpdated
            | RuntimeEventKind::RunCancelled
            | RuntimeEventKind::PlanRevisionCommitted
            | RuntimeEventKind::TaskStarted
            | RuntimeEventKind::TaskCompleted
            | RuntimeEventKind::TaskFailed
            | RuntimeEventKind::TaskCancelled
            | RuntimeEventKind::TaskTimedOut
            | RuntimeEventKind::TaskSkipped
            | RuntimeEventKind::TaskBlocked
            | RuntimeEventKind::TodoUpdated
            | RuntimeEventKind::BackgroundCellStarted
            | RuntimeEventKind::BackgroundCellFinished
            | RuntimeEventKind::RunContinuationConfigured
            | RuntimeEventKind::RunTurnStarted
            | RuntimeEventKind::RunTurnUsageAccounted
            | RuntimeEventKind::RunTurnCompacted
            | RuntimeEventKind::RunTurnFinished
            | RuntimeEventKind::RunProviderRetryScheduled
            | RuntimeEventKind::RunContinuationDeferred
            | RuntimeEventKind::RunContinuationResumed
            | RuntimeEventKind::RunPauseReasonChanged
    ) || (event.event_type == RuntimeEventKind::Note
        && matches!(note_kind, Some("summary_persisted")))
}

fn validate_event_suffix(
    run_id: &str,
    checkpoint_seq: i64,
    events: &[RuntimeTaskEvent],
) -> Result<(), ShadowError> {
    let mut expected = checkpoint_seq
        .checked_add(1)
        .ok_or_else(|| ShadowError::Rebuild("checkpoint seq overflow".to_string()))?;
    for event in events {
        if event.run_id != run_id {
            return Err(ShadowError::Rebuild(format!(
                "event {} belongs to run {}, expected {run_id}",
                event.seq, event.run_id
            )));
        }
        if event.seq != expected {
            return Err(ShadowError::Rebuild(format!(
                "event suffix is not contiguous: expected {expected}, got {}",
                event.seq
            )));
        }
        expected = expected
            .checked_add(1)
            .ok_or_else(|| ShadowError::Rebuild("event seq overflow".to_string()))?;
    }
    Ok(())
}

fn decode_event_text(text: &str) -> Result<Vec<RuntimeTaskEvent>, ShadowError> {
    let mut events = Vec::new();
    let has_terminal_newline = text.ends_with('\n');
    let lines = text.split('\n').collect::<Vec<_>>();
    let line_count = lines.len();
    for (index, line) in lines.into_iter().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event = match serde_json::from_str(line) {
            Ok(event) => event,
            Err(_) if !has_terminal_newline && index.saturating_add(1) == line_count => break,
            Err(error) => {
                return Err(ShadowError::Decode(format!(
                    "line {}: {}",
                    index.saturating_add(1),
                    error
                )));
            }
        };
        events.push(event);
    }
    Ok(events)
}

fn projection_degraded(seq: i64, error: impl std::fmt::Display) -> ShadowError {
    ShadowError::CommittedProjectionDegraded {
        seq,
        detail: error.to_string(),
    }
}

fn read_last_event(path: &Path) -> Result<Option<RuntimeTaskEvent>, ShadowError> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ShadowError::Io(error.to_string())),
    };
    let file_len = file
        .metadata()
        .map_err(|error| ShadowError::Io(error.to_string()))?
        .len();
    if file_len == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(file_len.saturating_sub(1)))
        .map_err(|error| ShadowError::Io(error.to_string()))?;
    let mut final_byte = [0_u8; 1];
    file.read_exact(&mut final_byte)
        .map_err(|error| ShadowError::Io(error.to_string()))?;
    let line_end = if final_byte.first().copied() == Some(b'\n') {
        file_len.saturating_sub(1)
    } else {
        file_len
    };
    if line_end == 0 {
        return Ok(None);
    }
    let (_, line) = read_line_ending_at(&mut file, line_end)?;
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|error| ShadowError::Decode(format!("last event: {error}")))
}

fn read_line_ending_at(
    file: &mut std::fs::File,
    line_end: u64,
) -> Result<(u64, Vec<u8>), ShadowError> {
    use std::io::{Read, Seek, SeekFrom};

    const BLOCK_BYTES: u64 = 8 * 1024;
    let mut search_end = line_end;
    let line_start = loop {
        let block_start = search_end.saturating_sub(BLOCK_BYTES);
        let block_len = search_end.saturating_sub(block_start);
        let block_len =
            usize::try_from(block_len).map_err(|error| ShadowError::Read(error.to_string()))?;
        let mut block = vec![0_u8; block_len];
        file.seek(SeekFrom::Start(block_start))
            .map_err(|error| ShadowError::Io(error.to_string()))?;
        file.read_exact(&mut block)
            .map_err(|error| ShadowError::Io(error.to_string()))?;
        if let Some(position) = block.iter().rposition(|byte| *byte == b'\n') {
            let position =
                u64::try_from(position).map_err(|error| ShadowError::Read(error.to_string()))?;
            break block_start.saturating_add(position).saturating_add(1);
        }
        if block_start == 0 {
            break 0;
        }
        search_end = block_start;
    };
    let line_len = line_end.saturating_sub(line_start);
    let line_len =
        usize::try_from(line_len).map_err(|error| ShadowError::Read(error.to_string()))?;
    let mut line = vec![0_u8; line_len];
    file.seek(SeekFrom::Start(line_start))
        .map_err(|error| ShadowError::Io(error.to_string()))?;
    file.read_exact(&mut line)
        .map_err(|error| ShadowError::Io(error.to_string()))?;
    Ok((line_start, line))
}

fn repair_torn_tail(path: &Path) -> Result<(), ShadowError> {
    use std::io::{Read, Seek, SeekFrom, Write};

    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(ShadowError::Io(error.to_string())),
    };
    let file_len = file
        .metadata()
        .map_err(|error| ShadowError::Io(error.to_string()))?
        .len();
    if file_len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::Start(file_len.saturating_sub(1)))
        .map_err(|error| ShadowError::Io(error.to_string()))?;
    let mut final_byte = [0_u8; 1];
    file.read_exact(&mut final_byte)
        .map_err(|error| ShadowError::Io(error.to_string()))?;
    if final_byte.first().copied() == Some(b'\n') {
        return Ok(());
    }
    let (tail_start, tail) = read_line_ending_at(&mut file, file_len)?;
    if serde_json::from_slice::<RuntimeTaskEvent>(&tail).is_ok() {
        file.seek(SeekFrom::End(0))
            .map_err(|error| ShadowError::Io(error.to_string()))?;
        file.write_all(b"\n")
            .map_err(|error| ShadowError::Io(error.to_string()))?;
    } else {
        file.set_len(tail_start)
            .map_err(|error| ShadowError::Io(error.to_string()))?;
    }
    file.sync_all()
        .map_err(|error| ShadowError::Io(error.to_string()))?;
    sync_parent(path).map_err(|error| ShadowError::Io(error.to_string()))
}

#[derive(Debug, thiserror::Error)]
pub enum ShadowError {
    #[error("shadow read failed: {0}")]
    Read(String),
    #[error("shadow io: {0}")]
    Io(String),
    #[error("shadow encode: {0}")]
    Encode(String),
    #[error("shadow decode: {0}")]
    Decode(String),
    #[error("shadow rebuild: {0}")]
    Rebuild(String),
    #[error("event seq {seq} committed but projection refresh degraded: {detail}")]
    CommittedProjectionDegraded { seq: i64, detail: String },
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Write `bytes` to `path` atomically: write to a unique tmp file, fsync,
/// rename over `path`.
///
/// The tmp file name must be **unique per call** (not a fixed `.tmp` suffix):
/// `TaskRuntimeStore` does not serialize `rewrite_plan` across concurrent
/// tasks, so two concurrent `atomic_write`s on the same `plan.json` would race
/// on a shared `plan.json.tmp` — one rename would move the other's tmp away,
/// making the second `rename` fail with "No such file or directory". A unique
/// tmp name (pid + counter + nanos) eliminates the collision.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("tmp.{}.{}.{}", std::process::id(), ts, uniq));
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    sync_parent(path)
}

/// Append `bytes` (one JSONL line, including trailing newline) to `path`.
/// Creates the file if it does not exist. Uses `O_APPEND` so concurrent
/// appends (if any) do not interleave within a single write(2) call.
fn append_line(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    sync_parent(path)
}

fn sync_parent(path: &Path) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::event_rebuild::rebuild_plan_from_events;
    use crate::tasks::task_runtime::file_store::FileTaskStore;
    use crate::tasks::task_runtime::store::TaskRuntimeStore;
    use crate::tasks::task_runtime::types::{
        AttendedMode, DomainProfile, ExecutionMode, PlanRevision, PlanTask, PlanTaskKind,
        RuntimeEventKind, TaskPatch, TaskPlan, TaskRunStatus, TaskUpdateOperation,
        TaskUpdateRequest, TodoStatus,
    };
    use std::sync::Arc;

    fn task(id: &str, kind: PlanTaskKind) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            title: format!("task {id}"),
            description: format!("do {id}"),
            kind,
            agent_role: "code_reviewer".to_string(),
            domain_profile: DomainProfile::AiCoding,
            depends_on: Vec::new(),
            parallel_group: None,
            files: vec!["src/a.rs".to_string()],
            allowed_tools: vec!["read_file".to_string()],
            required_artifacts: Vec::new(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: TodoStatus::Pending,
            status_detail: None,
            claim: None,
            sort_order: 0,
        }
    }

    fn append_run_created(shadow: &FileTaskShadow, run_id: &str) -> Result<(), String> {
        shadow
            .append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": "checkpoint goal",
                    "goal_revision": 1,
                    "goal_sha256": crate::tasks::task_runtime::task_goal_sha256("checkpoint goal"),
                    "domain_profile": "general",
                    "workspace_id": "ws",
                    "conversation_id": "conversation",
                    "root_message_id": "message",
                    "route": "complex_runtime",
                    "attended_mode": "unattended",
                    "created_at": "2026-08-17T00:00:00Z",
                }),
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    #[test]
    fn checkpoint_warm_suffix_matches_full_rebuild() -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(tmp.path());
        append_run_created(&shadow, "checkpoint-run")?;

        let cold = shadow
            .rewrite_plan_with_stats("checkpoint-run")
            .map_err(|error| error.to_string())?;
        assert!(!cold.used_checkpoint);
        assert_eq!(cold.folded_events, 1);
        assert_eq!(cold.seq, 1);

        shadow
            .append_event_line(
                "checkpoint-run",
                None,
                None,
                RuntimeEventKind::RunContinuationConfigured,
                serde_json::json!({
                    "enabled": true,
                    "token_budget": 500,
                    "time_budget_seconds": 60,
                }),
            )
            .map_err(|error| error.to_string())?;
        let warm = shadow
            .rewrite_plan_with_stats("checkpoint-run")
            .map_err(|error| error.to_string())?;
        assert!(warm.used_checkpoint);
        assert_eq!(warm.folded_events, 1);
        assert_eq!(warm.seq, 2);
        let warm_state = shadow
            .read_run_state("checkpoint-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "warm run-state projection missing".to_string())?;

        std::fs::remove_file(shadow.checkpoint_path("checkpoint-run"))
            .map_err(|error| error.to_string())?;
        let cold_again = shadow
            .rewrite_plan_with_stats("checkpoint-run")
            .map_err(|error| error.to_string())?;
        assert!(!cold_again.used_checkpoint);
        assert_eq!(cold_again.folded_events, 2);
        let rebuilt_state = shadow
            .read_run_state("checkpoint-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "rebuilt run-state projection missing".to_string())?;
        assert_eq!(
            serde_json::to_value(warm_state).map_err(|error| error.to_string())?,
            serde_json::to_value(rebuilt_state).map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[test]
    fn checkpoint_retains_usage_and_compaction_deduplication() -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(tmp.path());
        append_run_created(&shadow, "dedupe-run")?;
        for (kind, payload) in [
            (
                RuntimeEventKind::RunTurnStarted,
                serde_json::json!({
                    "turn_id": "turn-1",
                    "ordinal": 0,
                    "origin": "continuation",
                    "transcript_visibility": "internal",
                }),
            ),
            (
                RuntimeEventKind::RunTurnUsageAccounted,
                serde_json::json!({
                    "event_id": "usage-1",
                    "turn_id": "turn-1",
                    "input_tokens": 2,
                    "output_tokens": 3,
                    "elapsed_seconds": 0,
                }),
            ),
            (
                RuntimeEventKind::RunTurnCompacted,
                serde_json::json!({"event_id": "compact-1", "turn_id": "turn-1"}),
            ),
        ] {
            shadow
                .append_event_line("dedupe-run", None, None, kind, payload)
                .map_err(|error| error.to_string())?;
        }
        shadow
            .rewrite_plan("dedupe-run")
            .map_err(|error| error.to_string())?;

        for (kind, payload) in [
            (
                RuntimeEventKind::RunTurnUsageAccounted,
                serde_json::json!({
                    "event_id": "usage-1",
                    "turn_id": "turn-1",
                    "input_tokens": 2,
                    "output_tokens": 3,
                    "elapsed_seconds": 0,
                }),
            ),
            (
                RuntimeEventKind::RunTurnCompacted,
                serde_json::json!({"event_id": "compact-1", "turn_id": "turn-1"}),
            ),
        ] {
            shadow
                .append_event_line("dedupe-run", None, None, kind, payload)
                .map_err(|error| error.to_string())?;
        }
        let warm = shadow
            .rewrite_plan_with_stats("dedupe-run")
            .map_err(|error| error.to_string())?;
        assert!(warm.used_checkpoint);
        assert_eq!(warm.folded_events, 2);
        let continuation = shadow
            .read_run_state("dedupe-run")
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "continuation projection missing".to_string())?;
        assert_eq!(continuation.tokens_used, 5);
        assert_eq!(continuation.compaction_count, 1);
        Ok(())
    }

    #[test]
    fn corrupt_checkpoint_is_discarded_and_rebuilt_from_events() -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(tmp.path());
        append_run_created(&shadow, "corrupt-checkpoint")?;
        shadow
            .rewrite_plan("corrupt-checkpoint")
            .map_err(|error| error.to_string())?;
        std::fs::write(
            shadow.checkpoint_path("corrupt-checkpoint"),
            b"{\"schema_version\":1,\"state_hash\":",
        )
        .map_err(|error| error.to_string())?;
        shadow
            .append_event_line(
                "corrupt-checkpoint",
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({"from": "pending", "to": "running"}),
            )
            .map_err(|error| error.to_string())?;

        let rebuilt = shadow
            .rewrite_plan_with_stats("corrupt-checkpoint")
            .map_err(|error| error.to_string())?;
        assert!(!rebuilt.used_checkpoint);
        assert_eq!(rebuilt.folded_events, 2);
        let run = shadow
            .read_run_state("corrupt-checkpoint")
            .map_err(|error| error.to_string())?
            .map(|state| state.run)
            .ok_or_else(|| "rebuilt run missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Running);
        let bytes = std::fs::read(shadow.checkpoint_path("corrupt-checkpoint"))
            .map_err(|error| error.to_string())?;
        let checkpoint = serde_json::from_slice::<RuntimeCheckpoint>(&bytes)
            .map_err(|error| error.to_string())?;
        checkpoint
            .validate("corrupt-checkpoint")
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn checkpoint_schema_hash_and_offset_mismatches_fall_back() -> Result<(), String> {
        for case in ["schema", "hash", "offset"] {
            let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let shadow = FileTaskShadow::new(tmp.path());
            let run_id = format!("invalid-{case}");
            append_run_created(&shadow, &run_id)?;
            shadow
                .rewrite_plan(&run_id)
                .map_err(|error| error.to_string())?;
            let path = shadow.checkpoint_path(&run_id);
            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
            let mut value = serde_json::from_slice::<serde_json::Value>(&bytes)
                .map_err(|error| error.to_string())?;
            match case {
                "schema" => {
                    let field = value
                        .get_mut("schema_version")
                        .ok_or_else(|| "schema field missing".to_string())?;
                    *field = serde_json::json!(999);
                }
                "hash" => {
                    let field = value
                        .get_mut("state_hash")
                        .ok_or_else(|| "state_hash field missing".to_string())?;
                    *field = serde_json::json!("invalid");
                }
                "offset" => {
                    let field = value
                        .get_mut("event_byte_offset")
                        .ok_or_else(|| "offset field missing".to_string())?;
                    let offset = field
                        .as_u64()
                        .ok_or_else(|| "offset was not a u64".to_string())?;
                    *field = serde_json::json!(offset.saturating_add(1));
                }
                _ => return Err(format!("unsupported checkpoint case: {case}")),
            }
            std::fs::write(
                &path,
                serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?,
            )
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
            let rebuilt = shadow
                .rewrite_plan_with_stats(&run_id)
                .map_err(|error| error.to_string())?;
            assert!(!rebuilt.used_checkpoint, "case {case}");
            assert_eq!(rebuilt.folded_events, 2, "case {case}");
            let repaired = std::fs::read(&path).map_err(|error| error.to_string())?;
            serde_json::from_slice::<RuntimeCheckpoint>(&repaired)
                .map_err(|error| error.to_string())?
                .validate(&run_id)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    #[test]
    fn durable_event_reports_typed_projection_degradation() -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(tmp.path());
        append_run_created(&shadow, "degraded-run")?;
        shadow
            .rewrite_plan("degraded-run")
            .map_err(|error| error.to_string())?;
        std::fs::remove_file(shadow.checkpoint_path("degraded-run"))
            .map_err(|error| error.to_string())?;
        std::fs::create_dir(shadow.checkpoint_path("degraded-run"))
            .map_err(|error| error.to_string())?;
        let committed = shadow
            .append_event_line(
                "degraded-run",
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({"from": "pending", "to": "running"}),
            )
            .map_err(|error| error.to_string())?;
        let error = match shadow.rewrite_plan("degraded-run") {
            Ok(()) => {
                return Err("checkpoint directory unexpectedly accepted replacement".to_string());
            }
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ShadowError::CommittedProjectionDegraded { seq, .. } if seq == committed.seq
        ));
        assert_eq!(
            shadow
                .read_events("degraded-run")
                .map_err(|error| error.to_string())?
                .last()
                .map(|event| event.seq),
            Some(committed.seq)
        );

        std::fs::remove_dir(shadow.checkpoint_path("degraded-run"))
            .map_err(|error| error.to_string())?;
        shadow
            .rewrite_plan("degraded-run")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            shadow
                .read_run_state("degraded-run")
                .map_err(|error| error.to_string())?
                .map(|state| state.run.status),
            Some(TaskRunStatus::Running)
        );
        Ok(())
    }

    #[test]
    #[ignore = "M5 performance fixture; run explicitly with --ignored --nocapture"]
    fn benchmark_checkpoint_1k_turns_10k_events_100_compactions() -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(tmp.path());
        let run_id = "benchmark-run";
        std::fs::create_dir_all(shadow.run_dir(run_id)).map_err(|error| error.to_string())?;
        let mut events = Vec::with_capacity(10_000);
        {
            let mut push = |event_type: RuntimeEventKind, payload: serde_json::Value| {
                let next = i64::try_from(events.len())
                    .unwrap_or(i64::MAX)
                    .saturating_add(1);
                events.push(RuntimeTaskEvent {
                    seq: next,
                    run_id: run_id.to_string(),
                    task_id: None,
                    step_id: None,
                    event_type,
                    payload,
                    timestamp: chrono::Utc::now(),
                });
            };
            push(
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": "benchmark checkpoint",
                    "goal_revision": 1,
                    "goal_sha256": crate::tasks::task_runtime::task_goal_sha256("benchmark checkpoint"),
                    "domain_profile": "general",
                    "workspace_id": "ws",
                    "conversation_id": "benchmark",
                    "root_message_id": "message",
                    "route": "complex_runtime",
                    "attended_mode": "unattended",
                }),
            );
            push(
                RuntimeEventKind::RunContinuationConfigured,
                serde_json::json!({"enabled": true}),
            );
            for ordinal in 0_u64..1_000 {
                let turn_id = format!("turn-{ordinal}");
                push(
                    RuntimeEventKind::RunTurnStarted,
                    serde_json::json!({
                        "turn_id": turn_id.clone(),
                        "ordinal": ordinal,
                        "origin": "continuation",
                        "transcript_visibility": "internal",
                    }),
                );
                push(
                    RuntimeEventKind::RunTurnUsageAccounted,
                    serde_json::json!({
                        "event_id": format!("usage-{ordinal}"),
                        "turn_id": turn_id.clone(),
                        "input_tokens": 1,
                        "output_tokens": 1,
                        "elapsed_seconds": 0,
                    }),
                );
                if ordinal < 100 {
                    push(
                        RuntimeEventKind::RunTurnCompacted,
                        serde_json::json!({
                            "event_id": format!("compact-{ordinal}"),
                            "turn_id": turn_id.clone(),
                        }),
                    );
                }
                push(
                    RuntimeEventKind::RunTurnFinished,
                    serde_json::json!({
                        "turn_id": turn_id,
                        "status": "ended",
                        "elapsed_seconds": 0,
                        "made_progress": true,
                    }),
                );
            }
        }
        while events.len() < 10_000 {
            let next = i64::try_from(events.len())
                .unwrap_or(i64::MAX)
                .saturating_add(1);
            events.push(RuntimeTaskEvent {
                seq: next,
                run_id: run_id.to_string(),
                task_id: None,
                step_id: None,
                event_type: RuntimeEventKind::Note,
                payload: serde_json::json!({
                    "kind": "benchmark_runtime_diagnostic",
                    "detail": "representative persisted tool and runtime diagnostic payload used to avoid benchmarking an unrealistically empty event tail; the content is fixed so consecutive samples exercise identical JSON decoding and event-fold work",
                }),
                timestamp: chrono::Utc::now(),
            });
        }
        let mut jsonl = Vec::new();
        for event in &events {
            serde_json::to_writer(&mut jsonl, event).map_err(|error| error.to_string())?;
            jsonl.push(b'\n');
        }
        std::fs::write(shadow.events_path(run_id), &jsonl).map_err(|error| error.to_string())?;

        shadow
            .rewrite_plan_with_stats(run_id)
            .map_err(|error| error.to_string())?;
        let full_started = std::time::Instant::now();
        let full_events = shadow
            .read_events(run_id)
            .map_err(|error| error.to_string())?;
        let full_rebuilt =
            rebuild_plan_from_events(&full_events).map_err(|error| error.to_string())?;
        let full_elapsed = full_started.elapsed();
        assert_eq!(full_events.len(), 10_000);
        assert_eq!(full_rebuilt.run.run_id, run_id);
        let state = shadow
            .read_run_state(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "benchmark run-state missing".to_string())?;
        let continuation = state
            .continuation
            .ok_or_else(|| "benchmark continuation missing".to_string())?;
        assert_eq!(continuation.compaction_count, 100);
        assert_eq!(continuation.next_turn_ordinal, 1_000);

        let warm_rebuild_started = std::time::Instant::now();
        let checkpoint_result = shadow
            .load_checkpoint_suffix(run_id)
            .ok_or_else(|| "benchmark checkpoint missing".to_string())?
            .map_err(|error| error.to_string())?;
        let (checkpoint, suffix) = checkpoint_result;
        assert!(suffix.is_empty());
        let mut checkpoint_state = checkpoint.state;
        checkpoint_state.apply_events(&suffix);
        let warm_rebuilt = checkpoint_state
            .rebuilt_plan()
            .map_err(|error| error.to_string())?;
        let warm_rebuild_elapsed = warm_rebuild_started.elapsed();
        assert_eq!(warm_rebuilt.run.run_id, run_id);

        let append_fold_started = std::time::Instant::now();
        shadow
            .append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::RunContinuationDeferred,
                serde_json::json!({"reason": "benchmark"}),
            )
            .map_err(|error| error.to_string())?;
        let warm = shadow
            .rewrite_plan_with_stats(run_id)
            .map_err(|error| error.to_string())?;
        let append_fold_elapsed = append_fold_started.elapsed();
        assert!(warm.used_checkpoint);
        assert_eq!(warm.folded_events, 1);

        let snapshot_started = std::time::Instant::now();
        let run = FileTaskStore::new(shadow.clone())
            .get_run(run_id)
            .map_err(|error| error.to_string())?;
        let snapshot_elapsed = snapshot_started.elapsed();
        assert!(run.is_some());
        let checkpoint_bytes = std::fs::metadata(shadow.checkpoint_path(run_id))
            .map_err(|error| error.to_string())?
            .len();
        let event_bytes = std::fs::metadata(shadow.events_path(run_id))
            .map_err(|error| error.to_string())?
            .len();
        println!(
            "{}",
            serde_json::json!({
                "events": 10_001,
                "run_turns": 1_000,
                "compactions": 100,
                "full_rebuild_ms": full_elapsed.as_secs_f64() * 1_000.0,
                "warm_rebuild_ms": warm_rebuild_elapsed.as_secs_f64() * 1_000.0,
                "warm_append_fold_ms": append_fold_elapsed.as_secs_f64() * 1_000.0,
                "snapshot_read_ms": snapshot_elapsed.as_secs_f64() * 1_000.0,
                "checkpoint_bytes": checkpoint_bytes,
                "event_bytes": event_bytes,
                "warm_folded_events": warm.folded_events,
            })
        );
        assert!(checkpoint_bytes < event_bytes);
        assert!(full_elapsed < std::time::Duration::from_millis(150));
        assert!(warm_rebuild_elapsed < std::time::Duration::from_millis(10));
        assert!(append_fold_elapsed < std::time::Duration::from_millis(50));
        assert!(snapshot_elapsed < std::time::Duration::from_millis(2));
        assert!(
            full_elapsed.as_nanos() > warm_rebuild_elapsed.as_nanos().saturating_mul(5),
            "warm checkpoint rebuild must be at least five times faster"
        );
        assert!(checkpoint_bytes <= 128 * 1024);
        assert!(checkpoint_bytes.saturating_mul(10) < event_bytes);
        Ok(())
    }

    /// Parity: after attaching a shadow and driving a full lifecycle, the file
    /// mirror (events.jsonl + plan.json) must agree with the SQL store on events
    /// and on the rebuilt plan. This is the 0a acceptance gate for the shadow.
    #[test]
    fn shadow_parity_after_full_lifecycle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let shadow = Arc::new(FileTaskShadow::new(tmp.path()));
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).expect("store");

        // Drive a lifecycle.
        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "review runtime",
                "complex_runtime",
                AttendedMode::Attended,
            )
            .unwrap();
        let plan = TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::AiCoding,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("review runtime"),
            assumptions: vec!["small repo".to_string()],
            risks: vec!["flaky tests".to_string()],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                task("t1", PlanTaskKind::ReadOnlyReview),
                task("t2", PlanTaskKind::Investigation),
            ],
        };
        store.attach_plan_for_test(&plan).unwrap();
        store
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "rename task".to_string(),
                    operations: vec![TaskUpdateOperation::Update {
                        task_id: "t1".to_string(),
                        patch: TaskPatch {
                            title: Some("renamed t1".to_string()),
                            ..Default::default()
                        },
                    }],
                },
            )
            .unwrap();
        store
            .set_task_status("r1", "t1", TodoStatus::Running, Some("explorer"), None)
            .unwrap();

        // Parity 1: event count matches.
        let sql_events = store.list_events("r1", 0).unwrap();
        let file_events = shadow.read_events("r1").unwrap();
        assert_eq!(
            sql_events.len(),
            file_events.len(),
            "event count parity: sql={} file={}",
            sql_events.len(),
            file_events.len()
        );
        // Parity 2: event seqs + kinds match (payloads are enriched identically).
        for (s, f) in sql_events.iter().zip(file_events.iter()) {
            assert_eq!(s.seq, f.seq, "seq parity");
            assert_eq!(s.event_type, f.event_type, "kind parity at seq {}", s.seq);
        }

        // Parity 3: file plan.json matches SQL-rebuilt plan (via the shadow's own rebuild).
        let file_plan = shadow.read_plan("r1").unwrap().expect("plan.json written");
        let file_state = shadow
            .read_run_state("r1")
            .unwrap()
            .expect("run-state.json written");
        let rebuilt = rebuild_plan_from_events(&sql_events).unwrap();
        assert_eq!(file_state.run.run_id, rebuilt.run.run_id);
        assert_eq!(file_state.run.goal, rebuilt.run.goal);
        assert_eq!(file_state.run.route, rebuilt.run.route);
        assert_eq!(file_plan.plan_id, rebuilt.plan.plan_id);
        assert_eq!(file_plan.execution_mode, rebuilt.plan.execution_mode);
        assert_eq!(file_plan.tasks.len(), rebuilt.tasks.len());
        let ft1 = file_plan.tasks.iter().find(|t| t.id == "t1").unwrap();
        let rt1 = rebuilt.tasks.iter().find(|t| t.id == "t1").unwrap();
        assert_eq!(ft1.title, rt1.title);
        assert_eq!(ft1.title, "renamed t1");
        let state_t1 = file_state
            .tasks
            .iter()
            .find(|task| task.task_id == "t1")
            .unwrap();
        assert_eq!(state_t1.status, rt1.execution().status);
    }

    /// Helper: assert file shadow plan matches SQL-rebuilt plan on the fields the
    /// rebuilder tracks (run header, plan envelope, task count + per-task identity).
    fn assert_parity(store: &TaskRuntimeStore, shadow: &FileTaskShadow, run_id: &str) {
        let sql_events = store.list_events(run_id, 0).unwrap();
        let file_events = shadow.read_events(run_id).unwrap();
        assert_eq!(
            sql_events.len(),
            file_events.len(),
            "[{run_id}] event count parity: sql={} file={}",
            sql_events.len(),
            file_events.len()
        );
        for (s, f) in sql_events.iter().zip(file_events.iter()) {
            assert_eq!(s.seq, f.seq, "[{run_id}] seq parity");
            assert_eq!(
                s.event_type, f.event_type,
                "[{run_id}] kind parity at seq {}",
                s.seq
            );
        }
        let file_plan = shadow.read_plan(run_id).unwrap();
        let file_state = shadow.read_run_state(run_id).unwrap();
        let rebuilt = rebuild_plan_from_events(&sql_events).unwrap();
        let file_state = file_state.expect("run-state.json written");
        assert_eq!(
            file_state.run.run_id, rebuilt.run.run_id,
            "[{run_id}] run_id"
        );
        assert_eq!(file_state.run.goal, rebuilt.run.goal, "[{run_id}] goal");
        assert_eq!(file_state.run.route, rebuilt.run.route, "[{run_id}] route");
        assert_eq!(
            file_state.run.status, rebuilt.run.status,
            "[{run_id}] run status"
        );
        if rebuilt.plan.revision == 0 {
            assert!(file_plan.is_none(), "[{run_id}] no plan revision committed");
            return;
        }
        let file_plan = file_plan.expect("plan.json written");
        assert_eq!(
            file_plan.plan_id, rebuilt.plan.plan_id,
            "[{run_id}] plan_id"
        );
        assert_eq!(
            file_plan.tasks.len(),
            rebuilt.tasks.len(),
            "[{run_id}] task count"
        );
        for (ft, rt) in file_plan.tasks.iter().zip(rebuilt.tasks.iter()) {
            assert_eq!(ft.id, rt.id, "[{run_id}] task id");
            assert_eq!(ft.title, rt.title, "[{run_id}] task {} title", ft.id);
            assert_eq!(ft.kind, rt.kind, "[{run_id}] task {} kind", ft.id);
            let execution = file_state
                .tasks
                .iter()
                .find(|task| task.task_id == ft.id)
                .expect("task execution written");
            assert_eq!(
                execution.status,
                rt.execution().status,
                "[{run_id}] task {} status",
                ft.id
            );
            assert_eq!(
                ft.sort_order, rt.sort_order,
                "[{run_id}] task {} sort_order",
                ft.id
            );
        }
    }

    /// Parity across reorder + remove: ordering changes and deletions must be
    /// reflected identically in the file shadow and the SQL rebuild.
    #[test]
    fn shadow_parity_reorder_and_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path()));
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();

        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::General,
                "g",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        let plan = TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("g"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                task("t1", PlanTaskKind::Investigation),
                task("t2", PlanTaskKind::Investigation),
                task("t3", PlanTaskKind::Investigation),
            ],
        };
        store.attach_plan_for_test(&plan).unwrap();
        // Reorder: move t3 to front.
        store
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "prioritize t3".to_string(),
                    operations: vec![TaskUpdateOperation::Reorder {
                        task_ids: vec!["t3".to_string(), "t1".to_string(), "t2".to_string()],
                    }],
                },
            )
            .unwrap();
        assert_parity(&store, &shadow, "r1");
        store
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 2,
                    reason: "t2 is no longer required".to_string(),
                    operations: vec![TaskUpdateOperation::Skip {
                        task_id: "t2".to_string(),
                    }],
                },
            )
            .unwrap();
        assert_parity(&store, &shadow, "r1");
        let file_plan = shadow.read_plan("r1").unwrap().unwrap();
        assert_eq!(file_plan.tasks.len(), 3, "soft delete keeps the task");
        let _t2 = file_plan
            .tasks
            .iter()
            .find(|t| t.id == "t2")
            .expect("t2 still present after soft delete");
        let state = shadow.read_run_state("r1").unwrap().unwrap();
        let t2 = state
            .tasks
            .iter()
            .find(|task| task.task_id == "t2")
            .unwrap();
        assert_eq!(t2.status, echo_agent::tasks::TaskStatus::Skipped);
    }

    /// Parity across multiple runs: events and plans must not cross-contaminate.
    #[test]
    fn shadow_parity_multiple_runs_isolated() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path()));
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();

        for rid in ["r1", "r2"] {
            store
                .create_run(
                    rid,
                    "ws",
                    rid,
                    "m",
                    DomainProfile::AiCoding,
                    &format!("goal {rid}"),
                    "complex",
                    AttendedMode::Attended,
                )
                .unwrap();
            let plan = TaskPlan {
                plan_id: format!("p_{rid}"),
                run_id: rid.to_string(),
                revision: 1,
                domain_profile: DomainProfile::AiCoding,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256(&format!("goal {rid}")),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Parallel,
                tasks: vec![task(&format!("{rid}_t1"), PlanTaskKind::Summary)],
            };
            store.attach_plan_for_test(&plan).unwrap();
        }
        assert_parity(&store, &shadow, "r1");
        assert_parity(&store, &shadow, "r2");
        // r1's file plan must not contain r2's task.
        let p1 = shadow.read_plan("r1").unwrap().unwrap();
        assert!(p1.tasks.iter().all(|t| t.id == "r1_t1"));
    }

    /// Parity across run status transitions (Pending → Running → Paused → Completed).
    #[test]
    fn shadow_parity_run_status_transitions() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path()));
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();

        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::General,
                "g",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        store.transition_run("r1", TaskRunStatus::Running).unwrap();
        assert_parity(&store, &shadow, "r1");
        store.transition_run("r1", TaskRunStatus::Paused).unwrap();
        assert_parity(&store, &shadow, "r1");
        store.transition_run("r1", TaskRunStatus::Running).unwrap();
        store
            .transition_run("r1", TaskRunStatus::Completed)
            .unwrap();
        assert_parity(&store, &shadow, "r1");
        // Final status in file plan must be Completed.
        let state = shadow.read_run_state("r1").unwrap().unwrap();
        assert_eq!(state.run.status, TaskRunStatus::Completed);
    }

    // ── 0bc step-2: incremental append API (file becomes write authority) ──

    /// `append_event` writes one event line to events.jsonl with a seq derived
    /// from the current line count (1-based, monotonically increasing), and
    /// `rewrite_plan` rebuilds plan.json from the full event stream. This is
    /// the file-authority path that replaces SQL INSERT + flush_shadow.
    #[test]
    fn append_event_assigns_incremental_seq_and_rewinds_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = FileTaskShadow::new(tmp.path());

        // No RunCreated yet — append three events and check seq + plan rebuild.
        let e1 = shadow
            .append_event_line(
                "r1",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": "g", "domain_profile": "general",
                    "workspace_id": "ws", "conversation_id": "c1",
                    "root_message_id": "m1", "route": "", "created_at": "2026-06-25T00:00:00Z",
                }),
            )
            .unwrap();
        let e2 = shadow
            .append_event_line(
                "r1",
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                serde_json::json!({
                    "reason": "initial plan",
                    "base_revision": 0,
                    "skipped_task_ids": [],
                    "plan": PlanRevision {
                        plan_id: "p1".to_string(),
                        run_id: "r1".to_string(),
                        revision: 1,
                        domain_profile: DomainProfile::General,
                        goal_revision: 1,
                        goal_sha256: crate::tasks::task_runtime::task_goal_sha256("g"),
                        assumptions: Vec::new(),
                        risks: Vec::new(),
                        execution_mode: ExecutionMode::Parallel,
                        tasks: Vec::new(),
                    },
                }),
            )
            .unwrap();
        shadow.rewrite_plan("r1").unwrap();
        let e3 = shadow
            .append_event_line(
                "r1",
                Some("t1"),
                None,
                RuntimeEventKind::TaskStarted,
                serde_json::json!({ "status": "running", "owner_agent": "explorer" }),
            )
            .unwrap();

        // seq is 1-based, monotonically increasing per run.
        assert_eq!(e1.seq, 1);
        assert_eq!(e2.seq, 2);
        assert_eq!(e3.seq, 3);

        // read_events returns all three in seq order.
        let read_back = shadow.read_events("r1").unwrap();
        assert_eq!(read_back.len(), 3);
        assert_eq!(read_back[0].seq, 1);
        assert_eq!(read_back[2].event_type, RuntimeEventKind::TaskStarted);

        // rewrite_plan produces a plan.json reflecting the event stream.
        shadow.rewrite_plan("r1").unwrap();
        let plan = shadow.read_plan("r1").unwrap().unwrap();
        let state = shadow.read_run_state("r1").unwrap().unwrap();
        assert_eq!(state.run.run_id, "r1");
        assert_eq!(state.run.goal, "g");
        assert_eq!(plan.plan_id, "p1");
    }

    #[test]
    fn rewrite_plan_waits_for_same_run_write_lock() -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(tmp.path());
        shadow
            .append_event_line(
                "locked-run",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": "g",
                    "domain_profile": "general",
                    "workspace_id": "ws",
                    "conversation_id": "c1",
                    "root_message_id": "m1",
                    "route": "",
                    "created_at": "2026-06-25T00:00:00Z",
                }),
            )
            .map_err(|error| error.to_string())?;

        let run_lock = shadow.run_write_lock("locked-run");
        let guard = run_lock.lock().unwrap_or_else(|error| error.into_inner());
        let writer = shadow.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = writer
                .rewrite_plan("locked-run")
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });

        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Ok(_) => return Err("rewrite_plan proceeded without the same-run lock".to_string()),
            Err(error) => return Err(format!("rewrite result channel failed: {error}")),
        }

        drop(guard);
        rx.recv_timeout(std::time::Duration::from_secs(1))
            .map_err(|error| format!("rewrite_plan did not resume after unlock: {error}"))??;
        handle
            .join()
            .map_err(|_| "rewrite thread panicked".to_string())?;
        Ok(())
    }

    #[test]
    fn torn_tail_is_ignored_then_repaired_before_append() -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(tmp.path());
        shadow
            .append_event_line(
                "torn-run",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": "g",
                    "domain_profile": "general",
                    "workspace_id": "ws",
                    "conversation_id": "c1",
                    "root_message_id": "m1",
                    "route": "",
                    "created_at": "2026-08-14T00:00:00Z",
                }),
            )
            .map_err(|error| error.to_string())?;
        let path = shadow.events_path("torn-run");
        append_line(&path, b"{\"seq\":2").map_err(|error| error.to_string())?;

        assert_eq!(
            shadow
                .read_events("torn-run")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        let appended = shadow
            .append_event_line(
                "torn-run",
                Some("t1"),
                None,
                RuntimeEventKind::TaskStarted,
                serde_json::json!({"status": "running"}),
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(appended.seq, 2);
        assert_eq!(
            shadow
                .read_events("torn-run")
                .map_err(|error| error.to_string())?
                .len(),
            2
        );
        Ok(())
    }

    #[test]
    fn corruption_before_the_tail_still_fails_closed() -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = FileTaskShadow::new(tmp.path());
        let path = shadow.events_path("corrupt-run");
        let parent = path.parent().ok_or_else(|| "missing parent".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::write(&path, b"not-json\n{\"seq\":2").map_err(|error| error.to_string())?;

        assert!(matches!(
            shadow.read_events("corrupt-run"),
            Err(ShadowError::Decode(_))
        ));
        Ok(())
    }

    /// A second run's events must not perturb the first run's seq or plan —
    /// seq is per-run (each run has its own events.jsonl).
    #[test]
    fn append_event_seq_is_per_run() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = FileTaskShadow::new(tmp.path());

        let a1 = shadow
            .append_event_line(
                "rA",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({}),
            )
            .unwrap();
        let b1 = shadow
            .append_event_line(
                "rB",
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({}),
            )
            .unwrap();
        let a2 = shadow
            .append_event_line(
                "rA",
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({}),
            )
            .unwrap();

        assert_eq!(a1.seq, 1);
        assert_eq!(a2.seq, 2); // rA's second event, not affected by rB
        assert_eq!(b1.seq, 1); // rB starts at 1
    }

    /// Regression: concurrent `atomic_write`s on the same path must not collide
    /// on a shared tmp file name. Before the fix, `atomic_write` used a fixed
    /// `plan.json.tmp`; two concurrent writers raced on it — one `rename` moved
    /// the other's tmp away, so the second `rename` failed with
    /// "No such file or directory". This reproduced as the
    /// `failed to mark task running ... file shadow: shadow io: No such file`
    /// WARN spam during parallel readonly delegation (the executor fans out N tasks,
    /// each `set_task_status` → `rewrite_plan` → `atomic_write` concurrently).
    ///
    /// With per-call unique tmp names (pid + counter + nanos), renames never
    /// collide, so all concurrent writes succeed.
    #[test]
    fn concurrent_atomic_write_no_tmp_collision() {
        use std::sync::Arc;
        use std::thread;

        let tmpdir = tempfile::tempdir().expect("tempdir");
        let target = tmpdir.path().join("plan.json");

        // 8 threads × 50 iterations hammering atomic_write on the same path.
        // Before the fix this reliably produced "No such file" on a multicore
        // machine; after the fix every write succeeds.
        const THREADS: usize = 8;
        const ITERS: usize = 50;
        let target = Arc::new(target);
        let errors: Vec<std::io::Error> = (0..THREADS)
            .map(|t| {
                let target = target.clone();
                thread::spawn(move || {
                    let mut errs = Vec::new();
                    for i in 0..ITERS {
                        let payload = format!("t{t}-i{i}");
                        if let Err(e) = atomic_write(&target, payload.as_bytes()) {
                            errs.push(e);
                        }
                    }
                    errs
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|h| h.join().expect("thread"))
            .collect();

        assert!(
            errors.is_empty(),
            "concurrent atomic_write produced {} errors (expected 0); first: {:?}",
            errors.len(),
            errors.first()
        );

        // Final content is one of the writers' payloads (last rename wins) and
        // no stray tmp files are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(tmpdir.path())
            .expect("read_dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            leftovers,
            vec!["plan.json".to_string()],
            "only plan.json should remain; got {leftovers:?}"
        );
    }

    /// Regression: concurrent append_event_line callers for the same run_id
    /// must observe strictly unique, monotonically increasing seq values.
    /// Before the per-run write lock was added, two callers could both read
    /// the cached seq before either append landed, producing duplicate seq
    /// numbers in events.jsonl (observed in production as repeated seq
    /// 8/9/30/57/63-66). This test fires 100 concurrent appends and asserts
    /// the resulting seq set is exactly 1..=100.
    #[test]
    fn concurrent_append_produces_unique_strictly_increasing_seq() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let shadow = FileTaskShadow::new(tmp.path().to_path_buf());
        // Use a single shared run_id (the race only happens within one run).
        let run_id = "r-concurrent";
        shadow
            .append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({"goal": "x"}),
            )
            .expect("seed RunCreated");

        let threads = 8;
        let per_thread = 12; // 8 * 12 = 96, plus RunCreated → 97 total events
        let shadow = std::sync::Arc::new(shadow);
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let s = shadow.clone();
                std::thread::spawn(move || {
                    let mut local = Vec::new();
                    for i in 0..per_thread {
                        let ev = s
                            .append_event_line(
                                run_id,
                                Some(&format!("t{i}")),
                                None,
                                RuntimeEventKind::Note,
                                serde_json::json!({"i": i}),
                            )
                            .expect("append");
                        local.push(ev.seq);
                    }
                    local
                })
            })
            .collect();
        let mut all_seqs: Vec<i64> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("thread"))
            .collect();
        all_seqs.sort();

        // Each seq must appear exactly once.
        let mut seen = std::collections::HashSet::new();
        for &s in &all_seqs {
            assert!(seen.insert(s), "duplicate seq {s} observed");
        }
        // Seqs are 2..=(97), because RunCreated took seq=1.
        let expected: Vec<i64> = (2..=(1 + threads * per_thread) as i64).collect();
        assert_eq!(all_seqs, expected, "seq must be strictly contiguous");
    }
}
