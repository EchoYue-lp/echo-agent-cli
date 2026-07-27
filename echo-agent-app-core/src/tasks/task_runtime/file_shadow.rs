//! File-based authoritative store (U1c phase-0/0bc).
//!
//! The file system (`events.jsonl` + `plan.json`) is the read/write authority
//! for all task data. SQL was retired in 0bc step 5.
//!
//! Layout: `{root}/{run_id}/events.jsonl` (append-only) + `plan.json` (snapshot).
//! `root` defaults to `~/.eko/tasks/` (global, spec §2 path A).

use std::path::{Path, PathBuf};

use super::event_rebuild::rebuild_plan_from_events;
use super::types::{PlanRevision, RunStateSnapshot, RuntimeEventKind, RuntimeTaskEvent};

/// Shadow writer for one root directory. Cheap to clone (wraps a root path; the
/// append lock is per-run via the `events.jsonl` file handle being held briefly).
#[derive(Clone)]
pub struct FileTaskShadow {
    root: PathBuf,
    /// In-memory seq cache (run_id → last assigned seq) to avoid re-reading
    /// the whole `events.jsonl` on every append (O(n) per write → O(n²) per
    /// run). Seeded lazily from the file's line count on first append per run,
    /// so it self-heals across restarts. Shared across clones via `Arc` so all
    /// clones of this shadow agree on seq. Contention is low: the store holds
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
}

impl FileTaskShadow {
    /// Create a shadow rooted at `root`. The directory is created lazily on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            seq_cache: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_write_locks: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    /// Default shadow root: `~/.eko/tasks/`.
    pub fn default_root() -> PathBuf {
        echo_agent::paths::user_data_path("tasks")
    }

    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.root.join(run_id)
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

    // ── 0bc step-2: file-authority write path (replaces SQL INSERT + flush) ──

    /// Append one enriched event to `events.jsonl` for `run_id`, assigning it
    /// the next seq (1-based, = current line count + 1). Returns the fully
    /// formed `RuntimeTaskEvent` (with seq + timestamp) that was written.
    ///
    /// This is the file-authority write primitive: store write methods call
    /// this instead of `INSERT INTO tr_events`, then call [`rewrite_plan`] to
    /// refresh the `plan.json` snapshot. seq is per-run (each run has its own
    /// `events.jsonl`), so appending to run B does not advance run A's seq.
    ///
    /// Atomicity: the append is serialized by the store mutex (single writer),
    /// and the line is written with a trailing newline. A crash mid-append can
    /// at worst lose the last partial line — `read_events` skips empty lines
    /// and a future hardening pass (gate 2) will truncate a partial tail.
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

        // seq = last assigned + 1 (1-based). Cached in memory per run to avoid
        // re-reading events.jsonl on every append; seeded from the file's line
        // count on first touch per run so it self-heals across restarts.
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
        // `append_event_line` releases its guard before returning, so this is
        // not a re-entrant lock acquisition. Wait for any same-run append or
        // rewrite to finish; proceeding after `try_lock` returns WouldBlock
        // would rebuild and rename plan.json without serialization.
        let lock = self.run_write_lock(run_id);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());

        let events = self.read_events(run_id)?;
        if events.is_empty() {
            return Ok(());
        }
        let Some(latest) = events.last() else {
            return Ok(());
        };
        let note_kind = latest.payload.get("kind").and_then(|value| value.as_str());
        let affects_plan = latest.event_type == RuntimeEventKind::PlanRevisionCommitted;
        let affects_run_state = matches!(
            latest.event_type,
            RuntimeEventKind::RunCreated
                | RuntimeEventKind::RunStatusChanged
                | RuntimeEventKind::RunAttachmentsUpdated
                | RuntimeEventKind::RunCancelled
                | RuntimeEventKind::PlanRevisionCommitted
                | RuntimeEventKind::TaskStarted
                | RuntimeEventKind::TaskCompleted
                | RuntimeEventKind::TaskFailed
                | RuntimeEventKind::TaskSkipped
                | RuntimeEventKind::TaskBlocked
                | RuntimeEventKind::TodoUpdated
        ) || (latest.event_type == RuntimeEventKind::Note
            && matches!(note_kind, Some("summary_persisted")));
        if !affects_plan && !affects_run_state {
            return Ok(());
        }
        let rebuilt = match rebuild_plan_from_events(&events) {
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
                return Ok(());
            }
        };
        std::fs::create_dir_all(self.run_dir(run_id))
            .map_err(|e| ShadowError::Io(e.to_string()))?;
        if affects_plan {
            let plan_json = serde_json::to_string_pretty(&rebuilt.plan_revision())
                .map_err(|e| ShadowError::Encode(e.to_string()))?;
            atomic_write(&self.plan_path(run_id), plan_json.as_bytes())
                .map_err(|e| ShadowError::Io(e.to_string()))?;
        }
        if affects_run_state {
            let state_json = serde_json::to_string_pretty(&rebuilt.run_state())
                .map_err(|e| ShadowError::Encode(e.to_string()))?;
            atomic_write(&self.run_state_path(run_id), state_json.as_bytes())
                .map_err(|e| ShadowError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// Compute the next seq for `run_id`: last assigned + 1 (1-based).
    ///
    /// Uses the in-memory `seq_cache` to avoid re-reading `events.jsonl` on
    /// every append. On first touch per run (cache miss) it seeds from the
    /// file's line count, so the cache self-heals across restarts or when a
    /// different process wrote to the same file. Returns 1 if the file does
    /// not exist yet.
    fn next_seq(&self, run_id: &str) -> Result<i64, ShadowError> {
        // Fast path: cached.
        if let Ok(cache) = self.seq_cache.lock()
            && let Some(&last) = cache.get(run_id)
        {
            return Ok(last + 1);
        }
        // Slow path: seed from file line count (self-healing).
        let file_len = self.read_events(run_id)?.len() as i64;
        Ok(file_len + 1)
    }

    /// Enumerate every run_id known to the file store: the directory names
    /// under `root` that contain an `events.jsonl` (or `plan.json`). Used by
    /// the collection-query read API (`list_runs` / `list_runs_in` / etc.) that
    /// replaces SQL `SELECT ... FROM tr_runs WHERE ...`.
    pub fn list_run_ids(&self) -> Result<Vec<String>, ShadowError> {
        let mut ids = Vec::new();
        let read_dir = match std::fs::read_dir(&self.root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(ShadowError::Io(e.to_string())),
        };
        for entry in read_dir {
            let entry = entry.map_err(|e| ShadowError::Io(e.to_string()))?;
            let path = entry.path();
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

    /// The root path this shadow writes under (used by FileTaskStore to share
    /// the same root for enumeration).
    pub fn root_path(&self) -> &Path {
        &self.root
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
        let mut out = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let ev: RuntimeTaskEvent = serde_json::from_str(line)
                .map_err(|e| ShadowError::Decode(format!("line {}: {}", i + 1, e)))?;
            out.push(ev);
        }
        Ok(out)
    }
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
    std::fs::rename(&tmp, path)?;
    Ok(())
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::store::TaskRuntimeStore;
    use crate::tasks::task_runtime::types::{
        AttendedMode, DomainProfile, ExecutionMode, PlanPatchOperation, PlanPatchRequest,
        PlanRevision, PlanTask, PlanTaskKind, RuntimeEventKind, TaskPatch, TaskPlan, TaskRunStatus,
        TodoStatus,
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
            goal: "review runtime".to_string(),
            assumptions: vec!["small repo".to_string()],
            risks: vec!["flaky tests".to_string()],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                task("t1", PlanTaskKind::ReadOnlyReview),
                task("t2", PlanTaskKind::Investigation),
            ],
        };
        store.attach_plan(&plan).unwrap();
        store
            .patch_plan(
                "r1",
                &PlanPatchRequest {
                    base_revision: 1,
                    reason: "rename task".to_string(),
                    operations: vec![PlanPatchOperation::Update {
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
            goal: "g".to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                task("t1", PlanTaskKind::Investigation),
                task("t2", PlanTaskKind::Investigation),
                task("t3", PlanTaskKind::Investigation),
            ],
        };
        store.attach_plan(&plan).unwrap();
        // Reorder: move t3 to front.
        store
            .patch_plan(
                "r1",
                &PlanPatchRequest {
                    base_revision: 1,
                    reason: "prioritize t3".to_string(),
                    operations: vec![PlanPatchOperation::Reorder {
                        task_ids: vec!["t3".to_string(), "t1".to_string(), "t2".to_string()],
                    }],
                },
            )
            .unwrap();
        assert_parity(&store, &shadow, "r1");
        store
            .patch_plan(
                "r1",
                &PlanPatchRequest {
                    base_revision: 2,
                    reason: "t2 is no longer required".to_string(),
                    operations: vec![PlanPatchOperation::Skip {
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
                goal: format!("goal {rid}"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Parallel,
                tasks: vec![task(&format!("{rid}_t1"), PlanTaskKind::Summary)],
            };
            store.attach_plan(&plan).unwrap();
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
                        goal: "g".to_string(),
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
